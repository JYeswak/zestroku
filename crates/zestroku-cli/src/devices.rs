//! `zestroku devices [--no-ssdp] [--targets <file>] [--mx <secs>] [--json]`
//!
//! SSDP sweep ∪ targets.toml seeds, each endpoint probed for `device-info`.
//! `--no-ssdp` is the seeds-only run (flaky or black-holed multicast).
//! `--json` emits a machine-checkable object instead of the tab-separated
//! lines (schema in `tests/devices_json.rs` / docs/LIVE-VERIFY.md).

use std::path::PathBuf;
use std::process::ExitCode;

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_core::discovery::{parse_targets, reconcile};
use zestroku_core::ssdp_io::discover;
use zestroku_proto::{Transport, parse_device_info, query};
use zestroku_types::{Device, Error, Result};

use crate::transport::AsupersyncTransport;

struct Opts {
    no_ssdp: bool,
    targets: Option<PathBuf>,
    mx: u8,
    json: bool,
}

fn parse_opts(args: &[String]) -> std::result::Result<Opts, String> {
    let mut o = Opts {
        no_ssdp: false,
        targets: std::env::var_os("ZESTROKU_TARGETS").map(PathBuf::from),
        mx: 3,
        json: false,
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

async fn seeds_only(seeds: &str) -> Result<(Vec<Device>, Vec<String>)> {
    let (mut devices, mut notes) = (Vec::new(), Vec::new());
    for ep in parse_targets(seeds)? {
        let probed = async {
            let resp = AsupersyncTransport
                .send(&ep.host, ep.port, &query("device-info"))
                .await?;
            if resp.status != 200 {
                return Err(Error::Unreachable(format!("HTTP {}", resp.status)));
            }
            parse_device_info(&resp.body)
        }
        .await;
        match probed {
            Ok(info) => devices.push(reconcile(&ep, &info)),
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
    let result = runtime.block_on(async {
        if opts.no_ssdp {
            seeds_only(seeds.as_deref().unwrap_or_default()).await
        } else {
            discover(&AsupersyncTransport, seeds.as_deref(), opts.mx).await
        }
    });
    match result {
        Ok((devices, notes)) => {
            if opts.json {
                // Machine-checkable output for scripted/live verification
                // (docs/LIVE-VERIFY.md). Stable field order; notes carry
                // host/port + reason so a dead seed is assertable in JSON.
                let doc = serde_json::json!({
                    "devices": devices.iter().map(|d| serde_json::json!({
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
                for d in &devices {
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
