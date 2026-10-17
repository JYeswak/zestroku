//! `zestroku devices [--no-ssdp] [--targets <file>] [--mx <secs>] [--json]`
//!
//! Default: list cached daemon inventory without probing the LAN. Set
//! `ZESTROKU_DIRECT=1` for explicit discovery: SSDP ∪ targets.toml seeds,
//! `--no-ssdp` for seeds only, or `--scan <cidr>` for a bounded private scan.
//! `--json` emits a machine-checkable object instead of the tab-separated
//! lines (discovery contract exercised in `tests/devices_store.rs`).

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_core::ssdp_io::discover;
use zestroku_types::{ErrorCode, Result};

use crate::inventory;
use crate::store_db;
use crate::transport::SelectedTransport;

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

fn cached_inventory(json: bool) -> ExitCode {
    let devices = match crate::daemon_client::inventory::devices() {
        Ok(devices) => devices,
        Err(error) => return crate::report_read_error(error),
    };
    const NOTE: &str = "Cached daemon inventory, not a discovery sweep. Port and TV classification are not provided by this view; explicit ZESTROKU_DIRECT=1 discovery provides them.";
    eprintln!("zestroku devices: {NOTE}");
    if json {
        #[derive(serde::Serialize)]
        struct View<'a> {
            source: &'static str,
            devices: &'a [crate::daemon_client::inventory::InventoryDevice],
            notes: [&'static str; 0],
        }
        let mut output = std::io::stdout().lock();
        let result = serde_json::to_writer(
            &mut output,
            &View {
                source: "daemon",
                devices: &devices,
                notes: [],
            },
        )
        .map_err(std::io::Error::other)
        .and_then(|()| output.write_all(b"\n"));
        if let Err(error) = result {
            crate::report_cli_error(ErrorCode::OutputError, error, &[]);
            return ExitCode::FAILURE;
        }
    } else {
        for device in devices {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                device.host,
                device.name,
                device.model_number,
                device.software_version,
                device.access_tier,
            );
        }
    }
    ExitCode::SUCCESS
}

pub fn run(args: &[String]) -> ExitCode {
    if std::env::var("ZESTROKU_DIRECT").as_deref() != Ok("1") {
        let mut json = false;
        for argument in args {
            if argument == "--json" {
                json = true;
            } else {
                crate::report_cli_error(
                    ErrorCode::InvalidArgument,
                    format_args!("devices: discovery option {argument} requires ZESTROKU_DIRECT=1"),
                    &[],
                );
                return ExitCode::from(2);
            }
        }
        return cached_inventory(json);
    }
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            crate::report_cli_error(ErrorCode::InvalidArgument, e, &[]);
            return ExitCode::from(2);
        }
    };
    let seeds = match inventory::read_seeds(opts.targets.as_ref()) {
        Ok(s) => s,
        Err(e) => {
            crate::report_cli_error(ErrorCode::ConfigurationError, e, &[]);
            return ExitCode::FAILURE;
        }
    };
    let seeds = match &opts.scan {
        None => seeds,
        Some(cidr) => {
            let hosts = match zestroku_core::discovery::expand_cidr(cidr) {
                Ok(h) => h,
                Err(e) => {
                    crate::report_cli_error(ErrorCode::InvalidArgument, e, &[]);
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
        crate::report_cli_error(
            ErrorCode::InvalidArgument,
            "devices: --no-ssdp needs a targets.toml (--targets or ZESTROKU_TARGETS)",
            &[],
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
            crate::report_cli_error(ErrorCode::RuntimeUnavailable, e, &[]);
            return ExitCode::FAILURE;
        }
    };
    let result: Result<(Vec<inventory::FoundDevice>, Vec<String>)> = runtime.block_on(async {
        if opts.no_ssdp {
            inventory::seeds_only(seeds.as_deref().unwrap_or_default()).await
        } else {
            discover(&SelectedTransport, seeds.as_deref(), opts.mx).await
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
                    runtime
                        .block_on(inventory::seeds_only(&text))
                        .map(|(d, n)| {
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
            // Persist inventory through the shared producer (inventory.rs):
            // one DeviceRow per discovered device; a store failure must not
            // hide the devices (printed below regardless).
            let opened = store_db::open();
            let store_err = match &opened {
                Ok(s) => inventory::persist_inventory(Some(s), &devices),
                Err(e) => Some(format!("store unavailable: {e}")),
            };
            if let Some(note) = &store_err {
                crate::report_cli_error(ErrorCode::StoreUnavailable, note, &[]);
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
                crate::report_cli_error(
                    ErrorCode::DeviceUnreachable,
                    "devices: no devices found",
                    &[],
                );
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
            crate::report_cli_error(e.code(), &e, &[]);
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
