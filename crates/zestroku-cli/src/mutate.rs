//! Owner-approved CLI mutations use the resident daemon by default.
//!
//! The client forwards the opaque token; the daemon owns the snapshot,
//! authorization, dispatch, and completed audit result. `ZESTROKU_DIRECT=1`
//! explicitly selects the original diagnostic path with the same core gates.
//! Owner approval issuance still requires an interactive terminal.
//!
//! Loopback proofs live in tests/cli_daemon_mutations.rs and
//! tests/mutation_cli.rs; no simulator result claims a live-device mutation.
#![forbid(unsafe_code)]

mod daemon;

use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_core::mutation::{
    MutationContext, MutationError, authorize_mutation, finish_mutation, issue_approval,
};
use zestroku_proto::{
    Command, EcpGate, parse_active_app, parse_apps, parse_ecp_response, query, query_apps,
};
use zestroku_types::{AccessTier, Device, Error};

use crate::store_db;
use crate::transport::{AsupersyncTransport, send_selected};

struct MutationOpts {
    host: String,
    /// `launch`: app id. `key`: raw ECP key name. Empty when `--app` is used.
    target: String,
    /// `launch --app "<name>"`: resolve against the installed-app list (a4b).
    app: Option<String>,
    confirm: Option<String>,
}
/// Arg shapes: `launch <host> <app-id> [--confirm <token>]`,
/// `launch <host> --app "<name>" [--confirm <token>]` and
/// `key <host> <KEY> [--confirm <token>]`.
fn parse_opts(kind: &str, args: &[String]) -> std::result::Result<MutationOpts, String> {
    let mut it = args.iter();
    let host = it
        .next()
        .ok_or_else(|| format!("{kind} needs <host>"))?
        .clone();
    let mut target = String::new();
    let mut app = None;
    let mut confirm = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--confirm" => confirm = Some(it.next().ok_or("--confirm needs a token")?.clone()),
            "--app" => {
                if kind != "launch" {
                    return Err("--app needs launch".to_string());
                }
                if app.is_some() {
                    return Err("launch takes one --app".to_string());
                }
                app = Some(it.next().ok_or("--app needs a name")?.clone());
            }
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => {
                if !target.is_empty() {
                    return Err(format!("unexpected positional argument {other:?}"));
                }
                target = other.to_string();
            }
        }
    }
    if app.is_some() {
        if !target.is_empty() {
            return Err("launch takes <app-id> or --app, not both".to_string());
        }
    } else if target.is_empty() {
        return Err(format!("{kind} needs a target"));
    }
    Ok(MutationOpts {
        host,
        target,
        app,
        confirm,
    })
}

fn parse_app_id(target: &str) -> std::result::Result<u32, String> {
    target
        .parse()
        .map_err(|_| format!("app id must be a number, got {target:?}"))
}

fn build_command(kind: &str, target: &str) -> std::result::Result<Command, String> {
    match kind {
        "launch" => parse_app_id(target).map(zestroku_proto::launch),
        "key" => Ok(zestroku_proto::keypress_raw(target)),
        "install" if target == "sideload" => Ok(Command {
            method: zestroku_proto::Method::Post,
            path: "/plugin_install".to_string(),
            tier: AccessTier::Permissive,
        }),
        "install" => Err("install approval target must be sideload".to_string()),
        other => Err(format!("unknown mutation kind {other}")),
    }
}
/// Resolve an installed-app display name to its opaque id (a4b): a
/// case-insensitive exact match wins, else a unique case-insensitive prefix.
/// Zero matches yield APP_NOT_INSTALLED; several yield AMBIGUOUS_APP with the
/// candidate names. Matching touches display strings only, never ids.
fn resolve_app_id<'a>(
    apps: &'a [zestroku_proto::InstalledApp],
    name: &str,
) -> std::result::Result<&'a str, String> {
    let want = name.to_lowercase();
    if let Some(app) = apps.iter().find(|app| app.name.to_lowercase() == want) {
        return Ok(app.id.as_str());
    }
    let candidates: Vec<&str> = apps
        .iter()
        .filter(|app| app.name.to_lowercase().starts_with(&want))
        .map(|app| app.name.as_str())
        .collect();
    match candidates.as_slice() {
        [] => Err("APP_NOT_INSTALLED: no installed app matches the requested name".to_owned()),
        [only] => Ok(apps
            .iter()
            .find(|app| app.name == *only)
            .expect("prefix candidate comes from the installed list")
            .id
            .as_str()),
        many => Err(format!(
            "AMBIGUOUS_APP: requested name matches {} installed apps: {}",
            many.len(),
            many.join(", ")
        )),
    }
}

fn start_runtime() -> std::result::Result<asupersync::runtime::Runtime, String> {
    let reactor = create_reactor().map_err(|e| e.to_string())?;
    RuntimeBuilder::current_thread()
        .with_reactor(reactor)
        .build()
        .map_err(|e| e.to_string())
}

/// Exit code contract (extends main.rs: 3 = tier/403 refused locally):
/// 0 dispatched · 2 usage · 3 tier refused (audited) · 4 confirmation
/// required (audited) · 5 dispatch/wire failure.
pub fn run(kind: &str, args: &[String]) -> ExitCode {
    let opts = match parse_opts(kind, args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("zestroku: {e}");
            return ExitCode::from(2);
        }
    };
    if std::env::var("ZESTROKU_DIRECT").as_deref() != Ok("1") {
        return daemon::run(kind, &opts);
    }
    let (host, port) = crate::split_host_port(&opts.host);
    // Tier resolution (zestroku-tier-discovery-jt8 [c]): recorded tier from
    // the store, refreshed by `zestroku info`; ZESTROKU_TIER is only an
    // explicit, AUDITED override. Unknown refuses before any POST — the
    // message says to run `zestroku info`.
    let override_raw = std::env::var("ZESTROKU_TIER").ok();
    let store = match store_db::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("zestroku: action log unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let recorded = match zestroku_core::tier::recorded_tier_for(&store, &host, port) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("zestroku: cannot read recorded tier: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (tier_source, access_tier): (&'static str, AccessTier) = match override_raw.as_deref() {
        Some("permissive") | Some("Permissive") => ("env-override", AccessTier::Permissive),
        Some(other) if !other.trim().is_empty() && !other.eq_ignore_ascii_case("open") => {
            eprintln!("zestroku: ignoring unrecognized ZESTROKU_TIER={other:?}");
            match recorded {
                Some(t) => ("recorded", t.access_tier()),
                None => ("unknown-unprobed", AccessTier::Open),
            }
        }
        _ => match recorded {
            Some(t) => ("recorded", t.access_tier()),
            None => {
                eprintln!(
                    "zestroku: no recorded tier for {} — run `zestroku info {}` first (or set ZESTROKU_TIER=permissive explicitly)",
                    host, host
                );
                return ExitCode::from(3);
            }
        },
    };
    let device = Device {
        host: host.clone(),
        port,
        model_name: String::new(),
        model_number: String::new(),
        software_version: String::new(),
        is_tv: false,
        access_tier,
    };

    // Runtime + BEFORE snapshot come first: the audit row needs the
    // pre-mutation active app even when the gate refuses.
    let runtime = match start_runtime() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("zestroku: could not start asupersync runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    // `--app` name resolution (a4b): for the app arm the tier gate runs here
    // on local data (mirrors the apps verb), so a Limited device is refused
    // with no fetch. The installed list comes from one GET; absent/ambiguous
    // names exit 2 with zero POSTs before the confirmation gate.
    let (target, intent): (String, String) = match &opts.app {
        None => {
            let target = opts.target.clone();
            (target.clone(), format!("{kind} {target}"))
        }
        Some(name) => {
            let probe = query_apps();
            if let Err(error) = zestroku_core::check_tier(&probe, &device) {
                eprintln!("zestroku: {error}");
                return ExitCode::from(3);
            }
            let response = match runtime.block_on(send_selected(
                AsupersyncTransport,
                &device.host,
                device.port,
                &probe,
            )) {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("zestroku: cannot fetch installed apps: {error}");
                    return ExitCode::from(5);
                }
            };
            let apps = match parse_apps(&response.body) {
                Ok(apps) => apps,
                Err(error) => {
                    eprintln!("zestroku: cannot parse installed apps: {error}");
                    if matches!(error, Error::TierRefused { .. }) {
                        return ExitCode::from(3);
                    }
                    return ExitCode::from(5);
                }
            };
            let id = match resolve_app_id(&apps, name) {
                Ok(id) => id,
                Err(error) => {
                    eprintln!("zestroku: {error}");
                    return ExitCode::from(2);
                }
            };
            (id.to_owned(), format!("{kind} {id} (app \"{name}\")"))
        }
    };
    let command = match build_command(kind, &target) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("zestroku: {e}");
            return ExitCode::from(2);
        }
    };
    let host = device.host.clone();
    let before = runtime.block_on(async {
        match send_selected(
            AsupersyncTransport,
            &host,
            device.port,
            &query("active-app"),
        )
        .await
        {
            Ok(response) => parse_active_app(&response.body)
                .map(|app| app.name)
                .unwrap_or_else(|_| "unparseable".to_string()),
            Err(_) => "unreachable".to_string(),
        }
    });

    let context = MutationContext {
        client: "cli",
        surface: kind,
        intent: &intent,
        // Tier-source marker (zestroku-tier-discovery-jt8 [c]) prepended to
        // the snapshot; removing this marker turns the audit-row test red.
        before_state: &zestroku_core::tier::marked_before_state(tier_source, &before),
    };
    let token = opts.confirm.as_deref();
    let action_id = match authorize_mutation(&store, &device, &command, context, token) {
        Ok(id) => id,
        Err(MutationError::ConfirmationRequired { action_id, .. }) => {
            eprintln!(
                "zestroku: CONFIRMATION_REQUIRED — pass --confirm <token> to approve `{kind} {}` (audit id {action_id})",
                target
            );
            return ExitCode::from(4);
        }
        Err(MutationError::TierRefused(e)) => {
            eprintln!("zestroku: {e} (denial audited)");
            return ExitCode::from(3);
        }
        Err(MutationError::NotMutation) => {
            eprintln!("zestroku: internal: non-POST command reached the mutation gate");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("zestroku: mutation gate failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Approved: dispatch, then record the wire outcome either way.
    let dispatch = runtime.block_on(async {
        let response = send_selected(AsupersyncTransport, &device.host, device.port, &command)
            .await
            .map_err(|e| e.to_string())?;
        let classification = parse_ecp_response(&response);
        if classification.gate == EcpGate::Limited403 {
            return Err("device returned the Limited-mode 403 gate".to_string());
        }
        if classification.gate != EcpGate::Open {
            return Err(format!("unclassified response: {classification:?}"));
        }
        Ok::<(), String>(())
    });
    let outcome = match &dispatch {
        Ok(()) => format!("{kind} dispatched; after_state snapshot on next active-app read"),
        Err(e) => format!("dispatch failed: {e}"),
    };
    if let Err(e) = finish_mutation(&store, action_id, &outcome) {
        eprintln!("zestroku: warning: could not record outcome: {e}");
    }
    match dispatch {
        Ok(()) => {
            println!("ok: {kind} {target} (audit id {action_id})");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("zestroku: {e}");
            ExitCode::from(5)
        }
    }
}

/// Mint a one-use approval only from an interactive owner terminal.
pub fn approve(args: &[String]) -> ExitCode {
    let Some(verb) = args.first().map(String::as_str) else {
        eprintln!(
            "usage: zestroku approve <launch|key|install> <host> <target> [--installer-port <port>]"
        );
        return ExitCode::from(2);
    };
    let install = verb == "install";
    if (!install && args.len() != 3) || (install && args.len() != 3 && args.len() != 5) {
        eprintln!(
            "usage: zestroku approve <launch|key> <host> <target> | approve install <host> sideload [--installer-port <port>]"
        );
        return ExitCode::from(2);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("zestroku approve requires an interactive terminal");
        return ExitCode::from(2);
    }
    let host_label = &args[1];
    let target = &args[2];
    let (host, port) = if install {
        if target != "sideload" {
            eprintln!("zestroku: install approval target must be sideload");
            return ExitCode::from(2);
        }
        let port = if args.len() == 5 {
            if args[3] != "--installer-port" {
                eprintln!("zestroku: expected --installer-port <port>");
                return ExitCode::from(2);
            }
            match crate::install::parse_port(&args[4]) {
                Ok(port) => port,
                Err(error) => {
                    eprintln!("zestroku: {error}");
                    return ExitCode::from(2);
                }
            }
        } else {
            80
        };
        (host_label.clone(), port)
    } else {
        crate::split_host_port(host_label)
    };
    let command = match build_command(verb, target) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("zestroku: {error}");
            return ExitCode::from(2);
        }
    };
    let phrase = if install {
        format!("approve install {host_label} sideload --installer-port {port}")
    } else {
        format!("approve {verb} {host_label} {target}")
    };
    eprint!("Type exactly '{phrase}' to authorize this action: ");
    let _ = io::stderr().flush();
    let mut typed = String::new();
    if io::stdin().read_line(&mut typed).is_err() || typed.trim_end_matches(['\r', '\n']) != phrase
    {
        eprintln!("approval phrase did not match");
        return ExitCode::from(2);
    }
    let store = match store_db::open() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("zestroku: action log unavailable: {error}");
            return ExitCode::FAILURE;
        }
    };
    let device = Device {
        host,
        port,
        model_name: String::new(),
        model_number: String::new(),
        software_version: String::new(),
        is_tv: false,
        access_tier: AccessTier::Permissive,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64;
    match issue_approval(&store, &device, &command, now) {
        Ok(token) => {
            println!("{token}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("zestroku: {error}");
            ExitCode::FAILURE
        }
    }
}
#[cfg(test)]
mod key_command_tests {
    use super::*;

    #[test]
    fn key_commands_preserve_the_proto_ecp_wire_case() {
        let home = build_command("key", "Home").expect("build Home command");
        let modeled_home = zestroku_proto::keypress(zestroku_types::Key::Home);
        assert_eq!(home.path, modeled_home.path);
        assert_eq!(home.path, "/keypress/Home");

        let literal = build_command("key", "Lit_a").expect("build literal key command");
        assert_eq!(literal.path, "/keypress/Lit_a");
    }
}
