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

use std::path::PathBuf;
use std::process::ExitCode;

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_core::mutation::{
    self, MutationContext, MutationError, authorize_mutation, finish_mutation,
};
use zestroku_core::store::SqliteStore;
use zestroku_proto::{Command, EcpGate, Transport, parse_active_app, parse_ecp_response, query};
use zestroku_types::{AccessTier, Device, Error};

use crate::transport::AsupersyncTransport;

/// Audit trail location: the owner-LAN daemon DB. Overridable for tests.
fn store_path() -> PathBuf {
    std::env::var_os("ZESTROKU_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = PathBuf::from(
                std::env::var_os("HOME").unwrap_or_else(|| ".".into()),
            );
            p.push(".local/state/zestroku/actions.db");
            p
        })
}

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
            "--confirm" => {
                confirm =
                    Some(it.next().ok_or("--confirm needs a token")?.clone());
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(MutationOpts { host, target, confirm })
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
    let command = match build_command(kind, &opts.target) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("zestroku: {e}");
            return ExitCode::from(2);
        }
    };
    let device = Device {
        host: opts.host.clone(),
        port: 8060,
        model_name: String::new(),
        model_number: String::new(),
        software_version: String::new(),
        is_tv: false,
        access_tier: AccessTier::Open,
    };

    let store = match SqliteStore::open(&store_path()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("zestroku: action log unavailable: {e}");
            return ExitCode::FAILURE;
        }
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
        match AsupersyncTransport
            .send(&host, device.port, &query("active-app"))
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
        before_state: &before,
    };
    let confirmed = opts.confirm.as_deref().is_some_and(|t| !t.trim().is_empty());

    let action_id = match authorize_mutation(&store, &device, &command, context, confirmed) {
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
        Err(e @ MutationError::Store(_)) => {
            eprintln!("zestroku: action log write failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Approved: dispatch, then record the wire outcome either way.
    let dispatch = runtime.block_on(async {
        let response = AsupersyncTransport
            .send(&device.host, device.port, &command)
            .await?;
        let classification = parse_ecp_response(&response);
        if classification.gate == EcpGate::Limited403 {
            return Err("device returned the Limited-mode 403 gate".to_string());
        }
        if classification.gate != EcpGate::Open {
            return Err(format!("unclassified response: {classification:?}"));
        }
        Ok::<(), Error>(())
    });
    let outcome = match dispatch {
        Ok(()) => format!(
            "{kind} dispatched; after_state snapshot on next active-app read"
        ),
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
