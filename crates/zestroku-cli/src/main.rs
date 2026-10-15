//! zestroku-cli — the `zestroku` binary: CLI and on-demand daemon.
//!
//! Subcommands mirror the ECP surface. The core tier gate prevents
//! Permissive-tier commands from reaching a Limited-mode device; `info` and
//! `active-app` use the daemon by default.
#![forbid(unsafe_code)]

use std::{io::IsTerminal, process::ExitCode};

use zestroku_core::store::Store;
use zestroku_proto::{
    EcpGate, EcpResponse, parse_active_app, parse_device_info, parse_ecp_response, query,
};

mod aliases;
mod apps;
mod daemon_client;
mod devices;
mod install;
mod installer;
mod inventory;
mod mutate;
mod serve;
mod store_db;
mod transport;

fn usage() -> &'static str {
    r#"zestroku — owner-interop control for Roku devices

USAGE:
    zestroku devices                     list discovered devices
    zestroku alias <set|rm|ls> ...       manage local device names
    zestroku apps <host> [--json]        list installed apps (Permissive)
    zestroku info <host>                 device-info (Open tier)
    zestroku active-app <host>           focused app (Open tier)
    zestroku key <host> <KEY> [--confirm <token>]   keypress (Permissive)
    zestroku approve <verb> <host> <target> [--installer-port <port>]   owner TTY approval
    zestroku launch <host> <app-id> [--confirm <token>]  launch (Permissive)
    zestroku install <host> <zip> --password-stdin [--installer-port <port>] [--confirm <token>]
    zestroku mcp                         MCP server on stdio (agent tools)
    zestroku serve [--bind <ip:port>]    read-only HTTP API daemon (loopback default)

Install uses http://<host>:80/plugin_install by default; credentials are supplied through stdin, never arguments or the action log.
Read mode: info/active-app use ZESTROKU_DAEMON_URL (default http://127.0.0.1:8070); set ZESTROKU_DIRECT=1 for direct ECP.
A down daemon returns DAEMON_UNAVAILABLE; it never falls back to LAN.
Mutations require a single-use token minted by approve in an interactive owner terminal.
Tier behavior: Permissive-tier commands are refused locally (exit 3) when the device's observed tier is Limited — no round-trip, no 403."#
}

pub(crate) fn split_host_port(target: &str) -> (String, u16) {
    match target.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => match port.parse::<u16>() {
            Ok(port) => (host.to_owned(), port),
            Err(_) => (target.to_owned(), 8060),
        },
        _ => (target.to_owned(), 8060),
    }
}

fn resolve_cli_target(args: &mut [String]) -> Result<(), (ExitCode, String)> {
    let Some(verb) = args.first().map(String::as_str) else {
        return Ok(());
    };
    let (target_index, strict, mutation) = match verb {
        "info" | "active-app" if args.len() == 2 => (1, false, false),
        "apps" if args.len() >= 2 => {
            let Some((index, _)) = args
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, value)| !value.starts_with('-'))
            else {
                return Ok(());
            };
            (index, false, false)
        }
        "key" | "launch" if args.len() >= 3 => (1, false, true),
        "install" if args.len() >= 3 => (1, false, true),
        "approve" if args.len() >= 4 => {
            let mutation = matches!(args[1].as_str(), "key" | "launch" | "install");
            (2, false, mutation)
        }
        "alias" if args.get(1).is_some_and(|subcommand| subcommand == "set") && args.len() >= 4 => {
            (3, true, false)
        }
        _ => return Ok(()),
    };

    if verb == "approve" && (!std::io::stdin().is_terminal() || !std::io::stdout().is_terminal()) {
        return Ok(());
    }
    let store = store_db::open().map_err(|error| {
        (
            ExitCode::FAILURE,
            format!("target store unavailable: {error}"),
        )
    })?;
    let devices = store
        .list_devices()
        .map_err(|error| (ExitCode::FAILURE, format!("cannot list devices: {error}")))?;
    let aliases = store
        .list_aliases()
        .map_err(|error| (ExitCode::FAILURE, format!("cannot list aliases: {error}")))?;
    let smart_class = args[target_index].eq_ignore_ascii_case("tv")
        || args[target_index].eq_ignore_ascii_case("stick");
    match zestroku_core::targets::resolve_target(&args[target_index], &devices, &aliases) {
        Ok(device) => args[target_index] = device.id,
        Err(zestroku_core::targets::TargetResolutionError::NotFound { .. })
            if !strict && !smart_class => {}
        Err(error @ zestroku_core::targets::TargetResolutionError::AmbiguousName { .. }) => {
            if mutation {
                let surface = if verb == "approve" {
                    &args[1]
                } else {
                    &args[0]
                };
                let at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .min(i64::MAX as u64) as i64;
                let action = zestroku_core::store::ActionRow {
                    id: 0,
                    at,
                    client: "cli".into(),
                    surface: surface.to_owned(),
                    intent: args.join(" "),
                    decision: "denied".into(),
                    result: "AMBIGUOUS_NAME".into(),
                    before_state: "not queried: ambiguous target".into(),
                };
                store.insert_action(&action).map_err(|store_error| {
                    (
                        ExitCode::FAILURE,
                        format!("cannot audit ambiguous mutation: {store_error}"),
                    )
                })?;
                return Err((ExitCode::from(4), error.to_string()));
            }
            return Err((ExitCode::from(2), error.to_string()));
        }
        Err(error) if strict || smart_class => {
            return Err((ExitCode::from(2), error.to_string()));
        }
        Err(_) => {}
    }
    Ok(())
}
fn report_read_error(error: daemon_client::ReadError) -> ExitCode {
    eprintln!("zestroku: [{}] {error}", error.code());
    ExitCode::FAILURE
}

fn print_info(
    model_name: &str,
    model_number: &str,
    software_version: &str,
    is_tv: bool,
    access_tier: &str,
    tv_active_channel: Option<bool>,
) {
    let channel = match tv_active_channel {
        Some(true) => "present",
        Some(false) => "absent",
        None => "unknown",
    };
    println!(
        "{} ({}) software={} tv={} tier={} tv-active-channel={}",
        model_name, model_number, software_version, is_tv, access_tier, channel
    );
}

fn tv_channel_presence(response: &EcpResponse) -> Option<bool> {
    match parse_ecp_response(response).gate {
        EcpGate::Open | EcpGate::Limited403 => Some(true),
        EcpGate::Absent404 => Some(false),
        _ => None,
    }
}

fn run_direct_read(verb: &str, target: &str) -> ExitCode {
    let (host, port) = split_host_port(target);
    let command = match verb {
        "info" => query("device-info"),
        "active-app" => query("active-app"),
        _ => unreachable!("direct read dispatch only accepts info and active-app"),
    };
    let response = match daemon_client::direct_ecp_get(&host, port, &command) {
        Ok(response) => response,
        Err(error) => return report_read_error(error),
    };
    let classification = parse_ecp_response(&response);
    if classification.gate == EcpGate::Limited403 {
        eprintln!("zestroku: device returned the verified Limited-mode 403 gate");
        return ExitCode::from(3);
    }
    if classification.gate != EcpGate::Open {
        eprintln!(
            "zestroku: {} {} returned an unclassified response: {:?}",
            command.method, command.path, classification
        );
        return ExitCode::FAILURE;
    }

    if verb == "info" {
        let info = match parse_device_info(&response.body) {
            Ok(info) => info,
            Err(error) => {
                eprintln!("zestroku: {error}");
                return ExitCode::FAILURE;
            }
        };
        let apps = daemon_client::direct_ecp_get(&host, port, &query("apps"));
        let access_tier = match &apps {
            Ok(response) => zestroku_core::tier::classify_tier_from_response(response)
                .as_str()
                .to_owned(),
            Err(_) => "unknown".to_owned(),
        };
        if let Ok(response) = &apps
            && let Ok(store) = store_db::open()
        {
            let observed = zestroku_core::tier::classify_tier_from_response(response);
            let _ = zestroku_core::tier::record_probe(
                &store,
                &zestroku_core::tier::device_id(&host, port),
                observed,
                &format!("{}", parse_ecp_response(response).gate as i32),
            );
        }
        let tv_active_channel =
            match daemon_client::direct_ecp_get(&host, port, &query("tv-active-channel")) {
                Ok(response) => tv_channel_presence(&response),
                Err(_) => None,
            };
        print_info(
            &info.model_name,
            &info.model_number,
            &info.software_version,
            info.is_tv,
            &access_tier,
            tv_active_channel,
        );
    } else {
        let app = match parse_active_app(&response.body) {
            Ok(app) => app,
            Err(error) => {
                eprintln!("zestroku: {error}");
                return ExitCode::FAILURE;
            }
        };
        match app.app_id {
            Some(id) => println!("{} ({id})", app.name),
            None => println!("{}", app.name),
        }
    }
    ExitCode::SUCCESS
}

fn run_read_command(verb: &str, target: &str) -> ExitCode {
    if std::env::var("ZESTROKU_DIRECT").is_ok_and(|value| value == "1") {
        return run_direct_read(verb, target);
    }
    match verb {
        "info" => match daemon_client::info(target) {
            Ok(info) => {
                print_info(
                    &info.model_name,
                    &info.model_number,
                    &info.software_version,
                    info.is_tv,
                    &info.access_tier,
                    info.tv_active_channel,
                );
                ExitCode::SUCCESS
            }
            Err(error) => report_read_error(error),
        },
        "active-app" => match daemon_client::active_app(target) {
            Ok(app) => {
                match app.app_id {
                    Some(id) => println!("{} ({id})", app.name),
                    None => println!("{}", app.name),
                }
                ExitCode::SUCCESS
            }
            Err(error) => report_read_error(error),
        },
        _ => unreachable!("read dispatch only accepts info and active-app"),
    }
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Err((code, error)) = resolve_cli_target(&mut args) {
        eprintln!("zestroku: {error}");
        return code;
    }
    match args.first().map(String::as_str) {
        Some("devices") => devices::run(&args[1..]),
        Some("alias") => aliases::run(&args[1..]),
        Some("apps") => apps::run(&args[1..]),
        Some("serve") => serve::run(&args[1..]),
        Some("mcp") => match zestroku_mcp::run_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("zestroku mcp: {e}");
                ExitCode::from(1)
            }
        },
        Some("--version") | Some("version") => {
            println!(
                "{}",
                zestroku_core::version::version_string(env!("CARGO_PKG_VERSION"))
            );
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        None => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        Some("info") | Some("active-app") if args.len() == 2 => {
            run_read_command(&args[0], &args[1])
        }
        Some("approve") => mutate::approve(&args[1..]),
        Some("install") if args.len() >= 3 => install::run(&args[1..]),
        Some("launch") if args.len() >= 3 => mutate::run("launch", &args[1..]),
        Some("key") if args.len() >= 3 => mutate::run("key", &args[1..]),
        _ => {
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
    }
}
