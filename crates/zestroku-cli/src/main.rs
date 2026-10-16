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
mod complete;
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
    zestroku devices [--json]            list discovered devices
    zestroku alias <set|rm|ls> ...       manage local device names
    zestroku apps <host> [--json] [--daemon <url>] [--direct]
        list installed apps (Permissive)
    zestroku info <host> [--json] [--daemon <url>] [--direct]
        device-info (Open tier)
    zestroku active-app <host> [--json] [--daemon <url>] [--direct]
        focused app (Open tier)
    zestroku complete <devices|apps|verbs> [prefix]
        shell-completion candidates from local inventory/cache
    zestroku key <host> <KEY> [--confirm <token>]   keypress (Permissive)
    zestroku approve <verb> <host> <target> [--installer-port <port>]   owner TTY approval
    zestroku launch <host> <app-id> [--confirm <token>]  launch (Permissive)
    zestroku install <host> <zip> --password-stdin [--installer-port <port>] [--confirm <token>]
    zestroku mcp                         MCP server on stdio (agent tools)
    zestroku serve [--bind <ip:port>]    read-only HTTP API daemon (loopback default)

Install uses http://<host>:80/plugin_install by default; credentials are supplied through stdin, never arguments or the action log.
Read mode precedence: --direct, then --daemon <url>, then ZESTROKU_DIRECT=1 /
ZESTROKU_DAEMON_URL, then the loopback default (http://127.0.0.1:8070).
--direct and --daemon together are refused; flags always win over the
environment. A down daemon returns DAEMON_UNAVAILABLE; it never falls back
to LAN. Machine output (--json) goes to stdout only; diagnostics stay stderr.
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

/// Explicit read-path mode. Precedence is `--direct` > `--daemon <url>` >
/// `ZESTROKU_DIRECT=1` / `ZESTROKU_DAEMON_URL` > built-in default. A flag
/// always wins over the environment; `--direct` plus `--daemon` is a usage
/// error rather than a silent pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadMode {
    Default,
    Direct,
    Daemon(String),
}

/// Parsed `info` / `active-app` / `apps` read options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadOpts {
    target: String,
    json: bool,
    mode: ReadMode,
}

/// Parse read options over explicit inputs. `ZESTROKU_DIRECT=1` arrives as
/// `direct_env` (rather than being read here) so unit tests stay hermetic;
/// callers pass `std::env::var("ZESTROKU_DIRECT").ok()`. The daemon URL
/// environment stays resolved at call time inside daemon_client.
pub(crate) fn parse_read_opts(
    args: &[String],
    direct_env: Option<&str>,
) -> Result<ReadOpts, (ExitCode, String)> {
    let mut target: Option<String> = None;
    let mut json = false;
    let mut direct = false;
    let mut daemon: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => json = true,
            "--direct" => direct = true,
            "--daemon" => {
                index += 1;
                let Some(url) = args.get(index) else {
                    return Err((ExitCode::from(2), "read: --daemon needs a URL".into()));
                };
                daemon = Some(url.clone());
            }
            flag if flag.starts_with('-') => {
                return Err((ExitCode::from(2), format!("read: unknown flag {flag}")));
            }
            value if target.is_none() => target = Some(value.to_owned()),
            value => {
                return Err((
                    ExitCode::from(2),
                    format!("read: unexpected extra argument {value}"),
                ));
            }
        }
        index += 1;
    }
    let Some(target) = target else {
        return Err((ExitCode::from(2), "read: missing <host>".into()));
    };
    let mode = match (direct, daemon) {
        (true, Some(_)) => {
            return Err((
                ExitCode::from(2),
                "read: --direct conflicts with --daemon".into(),
            ));
        }
        (true, None) => ReadMode::Direct,
        (false, Some(url)) => ReadMode::Daemon(url),
        (false, None) if direct_env == Some("1") => ReadMode::Direct,
        (false, None) => ReadMode::Default,
    };
    Ok(ReadOpts { target, json, mode })
}

/// Flag-aware read-target scan shared by `info`, `active-app` and `apps`:
/// the first non-flag value past the verb, with `--daemon` consuming its
/// URL operand so the URL is never mistaken for the target.
fn read_target_index(args: &[String]) -> Option<usize> {
    let mut index = 1;
    let mut target = None;
    while index < args.len() {
        if args[index] == "--daemon" {
            index += 2;
            continue;
        }
        if !args[index].starts_with('-') && target.is_none() {
            target = Some(index);
        }
        index += 1;
    }
    target
}

fn resolve_cli_target(args: &mut [String]) -> Result<(), (ExitCode, String)> {
    let Some(verb) = args.first().map(String::as_str) else {
        return Ok(());
    };
    let (target_index, strict, mutation) = match verb {
        "info" | "active-app" | "apps" if args.len() >= 2 => {
            let Some(target_index) = read_target_index(args) else {
                return Ok(());
            };
            (target_index, false, false)
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

fn render_info(
    model_name: &str,
    model_number: &str,
    software_version: &str,
    is_tv: bool,
    access_tier: &str,
    tv_active_channel: Option<bool>,
    json: bool,
) -> String {
    if json {
        let channel = match tv_active_channel {
            Some(value) => serde_json::Value::Bool(value),
            None => serde_json::Value::Null,
        };
        serde_json::json!({
            "model_name": model_name,
            "model_number": model_number,
            "software_version": software_version,
            "is_tv": is_tv,
            "access_tier": access_tier,
            "tv_active_channel": channel,
        })
        .to_string()
    } else {
        let channel = match tv_active_channel {
            Some(true) => "present",
            Some(false) => "absent",
            None => "unknown",
        };
        format!(
            "{} ({}) software={} tv={} tier={} tv-active-channel={}",
            model_name, model_number, software_version, is_tv, access_tier, channel
        )
    }
}

fn render_active_app(name: &str, app_id: Option<u32>, json: bool) -> String {
    if json {
        serde_json::json!({ "name": name, "app_id": app_id }).to_string()
    } else {
        match app_id {
            Some(id) => format!("{name} ({id})"),
            None => name.to_owned(),
        }
    }
}

fn tv_channel_presence(response: &EcpResponse) -> Option<bool> {
    match parse_ecp_response(response).gate {
        EcpGate::Open | EcpGate::Limited403 => Some(true),
        EcpGate::Absent404 => Some(false),
        _ => None,
    }
}

fn run_direct_read(verb: &str, target: &str, json: bool) -> ExitCode {
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
        println!(
            "{}",
            render_info(
                &info.model_name,
                &info.model_number,
                &info.software_version,
                info.is_tv,
                &access_tier,
                tv_active_channel,
                json
            )
        );
    } else {
        let app = match parse_active_app(&response.body) {
            Ok(app) => app,
            Err(error) => {
                eprintln!("zestroku: {error}");
                return ExitCode::FAILURE;
            }
        };
        println!("{}", render_active_app(&app.name, app.app_id, json));
    }
    ExitCode::SUCCESS
}

fn run_read_command(verb: &str, opts: &ReadOpts) -> ExitCode {
    let direct_env = std::env::var("ZESTROKU_DIRECT").is_ok_and(|value| value == "1");
    let direct = opts.mode == ReadMode::Direct || (opts.mode == ReadMode::Default && direct_env);
    if direct {
        return run_direct_read(verb, &opts.target, opts.json);
    }
    // Daemon path: an explicit `--daemon` URL wins; otherwise the existing
    // environment/default resolution inside daemon_client applies.
    let via = match &opts.mode {
        ReadMode::Daemon(url) => Some(url.clone()),
        _ => None,
    };
    match verb {
        "info" => {
            let result = match via {
                Some(url) => daemon_client::info_via(&opts.target, &url),
                None => daemon_client::info(&opts.target),
            };
            match result {
                Ok(info) => {
                    println!(
                        "{}",
                        render_info(
                            &info.model_name,
                            &info.model_number,
                            &info.software_version,
                            info.is_tv,
                            &info.access_tier,
                            info.tv_active_channel,
                            opts.json
                        )
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => report_read_error(error),
            }
        }
        "active-app" => {
            let result = match via {
                Some(url) => daemon_client::active_app_via(&opts.target, &url),
                None => daemon_client::active_app(&opts.target),
            };
            match result {
                Ok(app) => {
                    println!("{}", render_active_app(&app.name, app.app_id, opts.json));
                    ExitCode::SUCCESS
                }
                Err(error) => report_read_error(error),
            }
        }
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
        Some("complete") => complete::run(&args[1..]),
        Some("info") | Some("active-app") => {
            let direct_env = std::env::var("ZESTROKU_DIRECT").ok();
            match parse_read_opts(&args[1..], direct_env.as_deref()) {
                Ok(opts) => run_read_command(&args[0], &opts),
                Err((code, error)) => {
                    eprintln!("zestroku: {error}");
                    code
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn read_opts_default_passes_target_through() {
        let opts = parse_read_opts(&args(&["192.0.2.10:8060"]), None).expect("parse");
        assert_eq!(opts.target, "192.0.2.10:8060");
        assert!(!opts.json);
        assert_eq!(opts.mode, ReadMode::Default);
    }

    #[test]
    fn read_opts_json_and_explicit_daemon_win_over_env() {
        let opts = parse_read_opts(
            &args(&["--json", "--daemon", "http://127.0.0.1:9999", "host"]),
            Some("1"),
        )
        .expect("parse");
        assert!(opts.json);
        assert_eq!(opts.mode, ReadMode::Daemon("http://127.0.0.1:9999".into()));
    }

    #[test]
    fn read_opts_direct_flag_beats_direct_env_off() {
        let opts = parse_read_opts(&args(&["--direct", "host"]), None).expect("parse");
        assert_eq!(opts.mode, ReadMode::Direct);
    }

    #[test]
    fn read_opts_direct_env_selects_direct_without_flags() {
        let opts = parse_read_opts(&args(&["host"]), Some("1")).expect("parse");
        assert_eq!(opts.mode, ReadMode::Direct);
    }

    #[test]
    fn read_opts_conflicting_flags_are_usage_errors() {
        let (code, _) = parse_read_opts(&args(&["--direct", "--daemon", "u", "h"]), None)
            .expect_err("conflict must fail");
        assert_eq!(code, ExitCode::from(2));
        let (code, _) = parse_read_opts(&args(&[]), None).expect_err("missing host must fail");
        assert_eq!(code, ExitCode::from(2));
        let (code, _) =
            parse_read_opts(&args(&["--daemon"]), None).expect_err("missing URL must fail");
        assert_eq!(code, ExitCode::from(2));
        let (code, _) =
            parse_read_opts(&args(&["--bogus", "h"]), None).expect_err("unknown flag must fail");
        assert_eq!(code, ExitCode::from(2));
    }

    #[test]
    fn render_info_json_shape_is_typed() {
        let document: serde_json::Value = serde_json::from_str(&render_info(
            "R",
            "N",
            "V",
            true,
            "permissive",
            Some(false),
            true,
        ))
        .expect("valid JSON");
        assert_eq!(document["model_name"], "R");
        assert_eq!(document["is_tv"], true);
        assert_eq!(document["access_tier"], "permissive");
        assert_eq!(document["tv_active_channel"], false);
    }

    #[test]
    fn render_info_json_absent_channel_is_null() {
        let document: serde_json::Value =
            serde_json::from_str(&render_info("R", "N", "V", false, "open", None, true))
                .expect("valid JSON");
        assert!(document["tv_active_channel"].is_null());
    }

    #[test]
    fn render_active_app_json_shape_is_typed() {
        let document: serde_json::Value =
            serde_json::from_str(&render_active_app("App", Some(7), true)).expect("valid JSON");
        assert_eq!(document["name"], "App");
        assert_eq!(document["app_id"], 7);
        let document: serde_json::Value =
            serde_json::from_str(&render_active_app("App", None, true)).expect("valid JSON");
        assert!(document["app_id"].is_null());
    }

    #[test]
    fn render_human_shapes_are_unchanged() {
        assert_eq!(
            render_info("R", "N", "V", true, "permissive", Some(true), false),
            "R (N) software=V tv=true tier=permissive tv-active-channel=present"
        );
        assert_eq!(render_active_app("App", Some(7), false), "App (7)");
        assert_eq!(render_active_app("App", None, false), "App");
    }

    #[test]
    fn read_target_scan_skips_daemon_url_for_alias() {
        // `apps --daemon URL living-room` must resolve the alias, never
        // mistake the URL operand for the target.
        assert_eq!(
            read_target_index(&args(&[
                "apps",
                "--daemon",
                "http://127.0.0.1:9",
                "living-room"
            ])),
            Some(3)
        );
        assert_eq!(
            read_target_index(&args(&["info", "--json", "living-room"])),
            Some(2)
        );
        assert_eq!(read_target_index(&args(&["info", "--json"])), None);
        assert_eq!(read_target_index(&args(&["apps", "--daemon"])), None);
    }
}
