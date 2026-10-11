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

mod apps;
mod devices;
mod mutate;
mod store_db;
mod transport;

use transport::{AsupersyncTransport, send_selected};

fn usage() -> &'static str {
    "zestroku — owner-interop control for Roku devices\n\
     \n\
     USAGE:\n\
     \x20   zestroku devices                     list discovered devices\n\
     \x20   zestroku apps <host> [--json]        list installed apps (Permissive)\n\
     \x20   zestroku info <host>                 device-info (Open tier)\n\
     \x20   zestroku active-app <host>           focused app (Open tier)\n\
     \x20   zestroku key <host> <KEY> [--confirm <token>]   keypress (Permissive)\n\
     \x20   zestroku launch <host> <app-id> [--confirm <token>]  launch (Permissive)\n\
     \n\
     Mutations are human-led: without --confirm they are refused locally\n\
     (exit 4), logged to the action log as denied, and nothing is sent.\n\
     Tier behavior: Permissive-tier commands are refused locally (exit 3)\n\
     when the device's observed tier is Limited — no round-trip, no 403."
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("devices") => devices::run(&args[1..]),
        Some("apps") => apps::run(&args[1..]),
        Some("--transport-probe") => {
            // Test-only surface (transport.rs selector_follows_env): prints
            // whether the std fallback is selected. Not documented usage.
            println!("{}", transport::use_std_fallback());
            ExitCode::SUCCESS
        }
        None => {
            println!("{}", usage());
            ExitCode::from(2)
        }
        Some("info") | Some("active-app") if args.len() == 2 => {
            let host = args[1].clone();
            let device = Device {
                host: host.clone(),
                port: 8060,
                model_name: String::new(),
                model_number: String::new(),
                software_version: String::new(),
                is_tv: false,
                access_tier: AccessTier::Open,
            };
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
                let response =
                    send_selected(AsupersyncTransport, &device.host, device.port, &cmd).await?;
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
                    // Tier observation (zestroku-tier-discovery-jt8 [b]):
                    // probe query/apps through the same transport, classify,
                    // record. A failed probe prints tier=unknown and does
                    // not fail `info` — device-info itself already succeeded.
                    let apps = send_selected(
                        AsupersyncTransport,
                        &device.host,
                        device.port,
                        &query("apps"),
                    )
                    .await;
                    let (tier_line, tv_channel) = {
                        let observed = match &apps {
                            Ok(response) => {
                                zestroku_core::tier::classify_tier_from_response(response)
                            }
                            Err(_) => zestroku_core::tier::ObservedTier::Unknown,
                        };
                        // tv-active-channel presence: 404 on the stick in
                        // both tiers (wire doc §2 surface fingerprint).
                        let tvch = send_selected(
                            AsupersyncTransport,
                            &device.host,
                            device.port,
                            &query("tv-active-channel"),
                        )
                        .await;
                        let tvch = match tvch {
                            Ok(response) => {
                                parse_ecp_response(&response).gate != EcpGate::Absent404
                            }
                            Err(_) => false,
                        };
                        (format!("tier={}", observed.as_str()), tvch)
                    };
                    // Record (best-effort; the store is the action-log DB).
                    if let Ok(response) = &apps {
                        if let Ok(store) = store_db::open() {
                            let observed =
                                zestroku_core::tier::classify_tier_from_response(response);
                            let _ = zestroku_core::tier::record_probe(
                                &store,
                                &device.host,
                                observed,
                                &format!("{}", parse_ecp_response(response).gate as i32),
                            );
                        }
                    }
                    println!(
                        "{} ({}) software={} tv={} {} tv-active-channel={}",
                        info.model_name,
                        info.model_number,
                        info.software_version,
                        info.is_tv,
                        tier_line,
                        if tv_channel { "present" } else { "absent" }
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
        Some("launch") if args.len() >= 3 => mutate::run("launch", &args[1..]),
        Some("key") if args.len() >= 3 => mutate::run("key", &args[1..]),
        _ => {
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
    }
}
