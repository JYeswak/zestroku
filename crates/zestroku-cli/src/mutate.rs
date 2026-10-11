//! Mutation subcommands (`launch`, `key`) — the m7e per-op confirmation
//! gate at the CLI boundary.
//!
//! Flow per bead zestroku-m7e-mutation-gate-svq, owner rules binding:
//! 1. Capture the active-app snapshot BEFORE anything else (before_state).
//! 2. `authorize_mutation` (zestroku-core): POST-only door → tier gate →
//!    confirmation. Every refusal writes an actions row; the CLI prints
//!    the audit id and exits 4 (5 = tier-refused, same audit).
//! 3. On approval: dispatch over the asupersync transport, then
//!    `finish_mutation` records the wire outcome. A blank/absent
//!    `--confirm` never dispatches.
//!
//! Offline proofs live in tests/mutation_cli.rs; the live launch leg is
//! Josh-gated per the dispatch queue.
#![forbid(unsafe_code)]

use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_core::mutation::{
    MutationContext, MutationError, authorize_mutation, finish_mutation, issue_approval,
};
use zestroku_proto::{Command, EcpGate, parse_active_app, parse_ecp_response, query};
use zestroku_types::{AccessTier, Device};

use crate::store_db;
use crate::transport::{AsupersyncTransport, send_selected};

struct MutationOpts {
    host: String,
    /// `launch`: app id. `key`: raw ECP key name.
    target: String,
    confirm: Option<String>,
}
/// Arg shapes: `launch <host> <app-id> [--confirm <token>]` and
/// `key <host> <KEY> [--confirm <token>]`.
fn parse_opts(kind: &str, args: &[String]) -> std::result::Result<MutationOpts, String> {
    let mut it = args.iter();
    let host = it
        .next()
        .ok_or_else(|| format!("{kind} needs <host>"))?
        .clone();
    let target = it
        .next()
        .ok_or_else(|| format!("{kind} needs a target"))?
        .clone();
    let mut confirm = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--confirm" => confirm = Some(it.next().ok_or("--confirm needs a token")?.clone()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(MutationOpts {
        host,
        target,
        confirm,
    })
}

fn build_command(kind: &str, target: &str) -> std::result::Result<Command, String> {
    match kind {
        "launch" => {
            let app_id: u32 = target
                .parse()
                .map_err(|_| format!("app id must be a number, got {target:?}"))?;
            Ok(zestroku_proto::launch(app_id))
        }
        "key" => Ok(zestroku_proto::keypress_raw(&target.to_uppercase())),
        other => Err(format!("unknown mutation kind {other}")),
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
    let (host, port) = crate::split_host_port(&opts.host);
    let command = match build_command(kind, &opts.target) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("zestroku: {e}");
            return ExitCode::from(2);
        }
    };
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
        intent: &format!("{kind} {}", opts.target),
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
                opts.target
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
            println!("ok: {kind} {} (audit id {action_id})", opts.target);
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
    if args.len() != 3 {
        eprintln!("usage: zestroku approve <launch|key> <host> <target>");
        return ExitCode::from(2);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("zestroku approve requires an interactive terminal");
        return ExitCode::from(2);
    }
    let verb = &args[0];
    let host_label = &args[1];
    let (host, port) = crate::split_host_port(host_label);
    let target = &args[2];
    let command = match build_command(verb, target) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("zestroku: {error}");
            return ExitCode::from(2);
        }
    };
    let phrase = format!("approve {verb} {host_label} {target}");
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
