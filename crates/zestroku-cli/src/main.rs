//! zestroku-cli — the `zestroku` binary: CLI now, `serve` daemon later.
//!
//! Subcommands mirror the ECP surface; the tier gate in zestroku-core runs
//! before any dispatch, so a Limited-mode device gets a clean local refusal
//! (never a 403 round-trip) for Permissive-tier commands.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_proto::{
    Command, EcpGate, Transport, parse_active_app, parse_device_info, parse_ecp_response, query,
};
use zestroku_types::{AccessTier, Device, Error};

mod devices;
mod transport;

use transport::AsupersyncTransport;

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
        Some("devices") => devices::run(&args[1..]),
        None => {
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
            if let Err(error) = zestroku_core::check_tier(&cmd, &device) {
                eprintln!("zestroku: {error}");
                return ExitCode::from(3);
            }
            let runtime = match create_reactor()
                .map_err(|error| error.to_string())
                .and_then(|reactor| {
                    RuntimeBuilder::current_thread()
                        .with_reactor(reactor)
                        .build()
                        .map_err(|error| error.to_string())
                }) {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("zestroku: could not start asupersync runtime: {error}");
                    return ExitCode::FAILURE;
                }
            };
            match runtime.block_on(async {
                let response = AsupersyncTransport
                    .send(&device.host, device.port, &cmd)
                    .await?;
                let classification = parse_ecp_response(&response);
                if classification.gate == EcpGate::Limited403 {
                    eprintln!("zestroku: device returned the verified Limited-mode 403 gate");
                    return Ok(ExitCode::from(3));
                }
                if classification.gate != EcpGate::Open {
                    return Err(Error::Unreachable(format!(
                        "{} {} returned an unclassified response: {:?}",
                        cmd.method, cmd.path, classification
                    )));
                }
                if args[0] == "info" {
                    let info = parse_device_info(&response.body)?;
                    println!(
                        "{} ({}) software={} tv={}",
                        info.model_name, info.model_number, info.software_version, info.is_tv
                    );
                } else {
                    let app = parse_active_app(&response.body)?;
                    match app.app_id {
                        Some(id) => println!("{} ({id})", app.name),
                        None => println!("{}", app.name),
                    }
                }
                Ok::<ExitCode, Error>(ExitCode::SUCCESS)
            }) {
                Ok(code) => code,
                Err(error) => {
                    eprintln!("zestroku: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("key") if args.len() == 3 => {
            let host = args[1].clone();
            let device = device_from_args(&host);
            let cmd = zestroku_proto::keypress_raw(&args[2].to_uppercase());
            if let Err(e) = zestroku_core::check_tier(&cmd, &device) {
                eprintln!("zestroku: {e}");
                return ExitCode::from(3);
            }
            eprintln!("zestroku: keypress requires the per-operation confirmation gate");
            ExitCode::FAILURE
        }
        _ => {
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
    }
}
