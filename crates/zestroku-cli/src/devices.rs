//! `zestroku devices [--no-ssdp] [--targets <file>] [--mx <secs>] [--json]`
//!
//! SSDP sweep ∪ targets.toml seeds, each endpoint probed for `device-info`.
//! `--no-ssdp` is the seeds-only run (flaky or black-holed multicast).
//! `--scan <cidr>` adds hosts that answer on the ECP port (private ranges
//! only, at most a /22): discovery that needs no multicast grant.
//! `--json` emits a machine-checkable object instead of the tab-separated
//! lines (schema in `tests/devices_json.rs` / docs/LIVE-VERIFY.md).

use std::path::PathBuf;
use std::process::ExitCode;

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_core::discovery::{identity_key, parse_targets, reconcile};
use zestroku_core::ssdp_io::discover;
use zestroku_core::store::Store;
use zestroku_proto::{parse_device_info, query};
use zestroku_types::{Device, Error, Result};

use crate::store_db;
use crate::transport::{AsupersyncTransport, send_selected};

#[path = "scan.rs"]
mod scan;

struct Opts {
    no_ssdp: bool,
    targets: Option<PathBuf>,
    mx: u8,
    json: bool,
    scan: Option<String>,
    scan_port: u16,
}

/// A discovered device plus its stable identity key (`identity_key`; empty
/// when unknown, which never merges). Named alias keeps signatures under
/// Clippy's type-complexity threshold.
type FoundDevice = (Device, String);

/// Record the first store failure only; later errors must not hide it.
fn note_first_error(store_err: &mut Option<String>, message: String) {
    if store_err.is_none() {
        *store_err = Some(message);
    }
}
fn parse_opts(args: &[String]) -> std::result::Result<Opts, String> {
    let mut o = Opts {
        no_ssdp: false,
        targets: std::env::var_os("ZESTROKU_TARGETS").map(PathBuf::from),
        mx: 3,
        json: false,
        scan: None,
        scan_port: zestroku_core::discovery::ECP_DEFAULT_PORT,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-ssdp" => o.no_ssdp = true,
            "--json" => o.json = true,
            "--targets" => {
                o.targets = Some(PathBuf::from(it.next().ok_or("--targets needs a path")?));
            }
            "--mx" => {
                o.mx = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|v| (1..=5).contains(v))
                    .ok_or("--mx needs 1..=5")?;
            }
            "--scan" => {
                o.scan = Some(it.next().ok_or("--scan needs a CIDR (a.b.c.d/n)")?.clone());
            }
            "--scan-port" => {
                o.scan_port = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|p| *p != 0)
                    .ok_or("--scan-port needs 1..=65535")?;
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

/// Seeds file text: explicit path (must exist), else the default path if present.
fn read_seeds(explicit: Option<&PathBuf>) -> std::result::Result<Option<String>, String> {
    if let Some(p) = explicit {
        return std::fs::read_to_string(p)
            .map(Some)
            .map_err(|e| format!("{}: {e}", p.display()));
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let p = PathBuf::from(home).join(".config/zestroku/targets.toml");
    Ok(std::fs::read_to_string(p).ok())
}

async fn seeds_only(seeds: &str) -> Result<(Vec<FoundDevice>, Vec<String>)> {
    let (mut devices, mut notes) = (Vec::new(), Vec::new());
    for ep in parse_targets(seeds)? {
        let probed = async {
            let resp = send_selected(
                AsupersyncTransport,
                &ep.host,
                ep.port,
                &query("device-info"),
            )
            .await?;
            if resp.status != 200 {
                return Err(Error::Unreachable(format!("HTTP {}", resp.status)));
            }
            parse_device_info(&resp.body)
        }
        .await;
        match probed {
            Ok(info) => devices.push((reconcile(&ep, &info), identity_key(&info))),
            Err(e) => notes.push(format!("{}:{}: {e}", ep.host, ep.port)),
        }
    }
    Ok((devices, notes))
}

pub fn run(args: &[String]) -> ExitCode {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("zestroku devices: {e}");
            return ExitCode::from(2);
        }
    };
    let seeds = match read_seeds(opts.targets.as_ref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("zestroku devices: {e}");
            return ExitCode::FAILURE;
        }
    };
    let seeds = match &opts.scan {
        None => seeds,
        Some(cidr) => {
            let hosts = match zestroku_core::discovery::expand_cidr(cidr) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("zestroku devices: {e}");
                    return ExitCode::from(2);
                }
            };
            let open = scan::open_hosts(&hosts, opts.scan_port, scan::CONNECT_TIMEOUT);
            eprintln!(
                "zestroku devices: scanned {} hosts, {} answered on port {}",
                hosts.len(),
                open.len(),
                opts.scan_port
            );
            let mut text = seeds.unwrap_or_default();
            text.push('\n');
            text.push_str(&scan::seeds_text(&open, opts.scan_port));
            Some(text)
        }
    };
    if opts.no_ssdp && seeds.is_none() {
        eprintln!(
            "zestroku devices: --no-ssdp needs a targets.toml (--targets or ZESTROKU_TARGETS)"
        );
        return ExitCode::from(2);
    }
    let runtime = match create_reactor().map_err(|e| e.to_string()).and_then(|r| {
        RuntimeBuilder::current_thread()
            .with_reactor(r)
            .build()
            .map_err(|e| e.to_string())
    }) {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("zestroku: could not start asupersync runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result: Result<(Vec<FoundDevice>, Vec<String>)> = runtime.block_on(async {
        if opts.no_ssdp {
            seeds_only(seeds.as_deref().unwrap_or_default()).await
        } else {
            discover(&AsupersyncTransport, seeds.as_deref(), opts.mx)
                .await
                .map(|(found, notes)| {
                    // SSDP-discovered devices carry no probed identity here;
                    // unknown identity never merges (fail-safe old behavior).
                    (
                        found.into_iter().map(|d| (d, String::new())).collect(),
                        notes,
                    )
                })
        }
    });
    // macOS gates UDP multicast per executable, so a fresh binary can find
    // nothing by SSDP while TCP to the ECP port works. When SSDP (plus any
    // seeds) found nothing and the caller named no scan, sweep this host's
    // own private /24 once, so a bare `zestroku devices` still works.
    let result = match result {
        Ok((devices, mut notes)) if devices.is_empty() && !opts.no_ssdp && opts.scan.is_none() => {
            match scan::local_cidr().and_then(|c| {
                zestroku_core::discovery::expand_cidr(&c)
                    .ok()
                    .map(|hosts| (c, hosts))
            }) {
                Some((cidr, hosts)) => {
                    let open = scan::open_hosts(&hosts, opts.scan_port, scan::CONNECT_TIMEOUT);
                    eprintln!(
                        "zestroku devices: SSDP found nothing; scanned {cidr} ({} hosts, {} answered on port {})",
                        hosts.len(),
                        open.len(),
                        opts.scan_port
                    );
                    let text = scan::seeds_text(&open, opts.scan_port);
                    runtime.block_on(seeds_only(&text)).map(|(d, n)| {
                        notes.extend(n);
                        (d, notes)
                    })
                }
                None => Ok((devices, notes)),
            }
        }
        other => other,
    };
    match result {
        Ok((devices, notes)) => {
            // Persist inventory (zestroku-tier-discovery-jt8 [d]): one
            // DeviceRow per discovered device, id = "host:port". Privacy:
            // model-name/number/version only; never serial, UDN, MAC,
            // device-id, advertising-id or the friendly name; the raw
            // device-info body is not retained. A store failure must not
            // hide the devices: they are printed below regardless, with a
            // typed note on stderr and a non-zero exit.
            let mut store_err: Option<String> = None;
            let opened = store_db::open();
            let store = match &opened {
                Ok(s) => Some(s),
                Err(e) => {
                    store_err = Some(format!("store unavailable: {e}"));
                    None
                }
            };
            let mut rows = Vec::with_capacity(devices.len());
            for (d, identity) in &devices {
                let id = zestroku_core::tier::device_id(&d.host, d.port);
                let access_tier = match store {
                    Some(s) => match zestroku_core::tier::recorded_tier_for(s, &d.host, d.port) {
                        Ok(Some(t)) => t.as_str().to_string(),
                        Ok(None) => "unknown".to_string(),
                        Err(e) => {
                            if store_err.is_none() {
                                store_err = Some(format!("tier lookup {id}: {e}"));
                            }
                            "unknown".to_string()
                        }
                    },
                    None => "unknown".to_string(),
                };
                rows.push((
                    zestroku_core::store::DeviceRow {
                        id: id.clone(),
                        name: d.model_name.clone(),
                        ip: d.host.clone(),
                        model: d.model_name.clone(),
                        model_num: d.model_number.clone(),
                        firmware: d.software_version.clone(),
                        wifi_driver: String::new(),
                        access_tier,
                        last_seen: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|t| t.as_secs() as i64)
                            .unwrap_or(0),
                    },
                    identity.clone(),
                ));
            }
            if let Some(s) = store {
                // Self-healing identity (zestroku-self-healing-identity-7ac):
                // when a known identity key reappears at a new endpoint, the
                // existing row moves (aliases follow it) instead of leaving
                // a stale duplicate. Unknown/empty identity keeps the old
                // per-endpoint behavior. Probe caches (apps, capabilities,
                // tiers) stay endpoint-keyed and are re-observed on demand.
                for (row, identity) in &rows {
                    if !identity.is_empty() {
                        match s.find_device_id_by_identity(identity) {
                            Ok(Some(owner)) if owner != row.id => {
                                if let Err(e) = s.rekey_aliases(&owner, &row.id) {
                                    note_first_error(
                                        &mut store_err,
                                        format!("rekey aliases {owner}: {e}"),
                                    );
                                    break;
                                }
                                if let Err(e) = s.delete_device(&owner) {
                                    note_first_error(
                                        &mut store_err,
                                        format!("retire moved row {owner}: {e}"),
                                    );
                                    break;
                                }
                                if let Err(e) = s.delete_device_identity(&owner) {
                                    note_first_error(
                                        &mut store_err,
                                        format!("retire moved identity {owner}: {e}"),
                                    );
                                    break;
                                }
                            }
                            Err(e) => {
                                note_first_error(
                                    &mut store_err,
                                    format!("identity lookup {}: {e}", row.id),
                                );
                                break;
                            }
                            Ok(_) => {}
                        }
                    }
                    if let Err(e) = s.upsert_device(row) {
                        note_first_error(&mut store_err, format!("upsert {}: {e}", row.id));
                        break;
                    }
                    if identity.is_empty() {
                        // Nothing to record; unknown identity never merges.
                    } else if let Err(e) = s.record_device_identity(&row.id, identity) {
                        note_first_error(
                            &mut store_err,
                            format!("record identity {}: {e}", row.id),
                        );
                        break;
                    }
                }
            }
            if let Some(note) = &store_err {
                eprintln!("zestroku devices: store note: {note}");
            }
            if opts.json {
                // Machine-checkable output for scripted/live verification
                // (docs/LIVE-VERIFY.md). Stable field order; notes carry
                // host/port + reason so a dead seed is assertable in JSON.
                let doc = serde_json::json!({
                    "devices": devices.iter().map(|(d, _)| serde_json::json!({
                        "host": d.host,
                        "port": d.port,
                        "model_name": d.model_name,
                        "model_number": d.model_number,
                        "software_version": d.software_version,
                        "is_tv": d.is_tv,
                    })).collect::<Vec<_>>(),
                    "notes": notes.iter().map(|n| {
                        let (target, reason) = match n.split_once(": ") {
                            Some((t, r)) => (t.to_string(), r.to_string()),
                            None => (n.clone(), String::new()),
                        };
                        serde_json::json!({ "target": target, "reason": reason })
                    }).collect::<Vec<_>>(),
                });
                println!("{doc:#}");
            } else {
                for (d, _) in &devices {
                    println!(
                        "{}:{}\t{}\t{}\tsoftware={}\ttv={}",
                        d.host, d.port, d.model_name, d.model_number, d.software_version, d.is_tv
                    );
                }
                for n in &notes {
                    eprintln!("zestroku devices: note: {n}");
                }
            }
            if devices.is_empty() {
                eprintln!("zestroku devices: no devices found");
                ExitCode::FAILURE
            } else if store_err.is_some() {
                // Devices were found and printed; the persist failure is
                // still real and must not read as a clean run.
                ExitCode::from(5)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("zestroku devices: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn option_parsing() {
        let o = parse_opts(&a(&["--no-ssdp", "--targets", "t.toml", "--mx", "2"])).unwrap();
        assert!(o.no_ssdp && o.mx == 2);
        assert!(parse_opts(&a(&["--mx", "9"])).is_err());
        assert!(parse_opts(&a(&["--bogus"])).is_err());
        assert!(parse_opts(&a(&["--targets"])).is_err());
    }
}
