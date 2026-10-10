//! zestroku-cli — the `zestroku` binary: CLI now, `serve` daemon later.
//!
//! Subcommands mirror the ECP surface; the tier gate in zestroku-core runs
//! before any dispatch, so a Limited-mode device gets a clean local refusal
//! (never a 403 round-trip) for Permissive-tier commands.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use zestroku_proto::{query, Command};
use zestroku_types::{AccessTier, Device};

fn usage() -> &'static str {
    "zestroku — owner-interop control for Roku devices\n\
     \n\
     USAGE:\n\
     \x20   zestroku devices                     list discovered devices\n\
     \x20   zestroku info <host>                 device-info (Open tier)\n\
     \x20   zestroku active-app <host>           focused app (Open tier)\n\
     \x20   zestroku key <host> <Key>            keypress (Permissive tier)\n\
     \n\
     Tier behavior: Permissive-tier commands are refused locally (exit 3)\n\
     when the device's observed tier is Limited — no round-trip, no 403."
}

fn device_from_args(host: &str) -> Device {
    // Skeleton: minimal device with observed Limited tier. The discovery
    // (M1) and inventory store (M7 core) wire the real values in.
    Device {
        host: host.to_string(),
        port: 8060,
        model_name: String::new(),
        model_number: String::new(),
        software_version: String::new(),
        is_tv: false,
        access_tier: AccessTier::Open,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("devices") | None => {
            println!("{}", usage());
            ExitCode::from(2)
        }
        Some("info") | Some("active-app") if args.len() == 2 => {
            let host = args[1].clone();
            let device = device_from_args(&host);
            let cmd: Command = if args[0] == "info" {
                query("device-info")
            } else {
                query("active-app")
            };
            if let Err(e) = zestroku_core::check_tier(&cmd, &device) {
                eprintln!("zestroku: {e}");
                return ExitCode::from(3);
            }
            // Transport wiring lands with the M7 daemon lane; the command
            // construction + tier gate are the tested surface today.
            println!("{} {} -> dispatch pending transport wiring", cmd.method, cmd.path);
            ExitCode::SUCCESS
        }
        Some("key") if args.len() == 3 => {
            let host = args[1].clone();
            let device = device_from_args(&host);
            let cmd = zestroku_proto::keypress_raw(&args[2].to_uppercase());
            if let Err(e) = zestroku_core::check_tier(&cmd, &device) {
                eprintln!("zestroku: {e}");
                return ExitCode::from(3);
            }
            println!("{} {} -> dispatch pending transport wiring", cmd.method, cmd.path);
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
    }
}
