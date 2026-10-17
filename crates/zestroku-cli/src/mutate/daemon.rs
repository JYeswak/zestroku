//! Default CLI mutations forward intent to the daemon, never consume approvals locally.

use std::process::ExitCode;

use crate::daemon_client::ReadError;
use crate::daemon_client::mutation::MutationClient;

use super::{MutationOpts, parse_app_id, resolve_app_id};

pub(super) fn run(kind: &str, opts: &MutationOpts) -> ExitCode {
    let client = match MutationClient::from_env() {
        Ok(client) => client,
        Err(error) => return report_error(error),
    };
    let result = match kind {
        "key" => client.key(&opts.host, &opts.target, opts.confirm.as_deref()),
        "launch" => {
            let app_id = if let Some(name) = opts.app.as_deref() {
                let apps = match client.apps(&opts.host) {
                    Ok(apps) => apps,
                    Err(error) => return report_error(error),
                };
                let target = match resolve_app_id(&apps, name) {
                    Ok(target) => target,
                    Err(error) => {
                        crate::report_cli_error(error.code(), &error, error.suggestions());
                        return ExitCode::from(2);
                    }
                };
                parse_app_id(target)
            } else {
                parse_app_id(&opts.target)
            };
            let app_id = match app_id {
                Ok(app_id) => app_id,
                Err(error) => return usage_error(&error),
            };
            client.launch(
                &opts.host,
                app_id,
                opts.app.as_deref(),
                opts.confirm.as_deref(),
            )
        }
        _ => return usage_error("only key and launch use the resident mutation client"),
    };
    match result {
        Ok(completed) => {
            println!(
                "dispatched: audit id {} ({} {})",
                completed.action_id, completed.surface, completed.target
            );
            ExitCode::SUCCESS
        }
        Err(error) => report_error(error),
    }
}

pub(super) fn undo_last(args: &[String]) -> ExitCode {
    let Some(host) = args.first() else {
        return usage_error("undo needs <host>");
    };
    let mut confirm: Option<&str> = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--confirm" => {
                if confirm.is_some() {
                    return usage_error("undo takes one --confirm token");
                }
                index += 1;
                let Some(token) = args.get(index) else {
                    return usage_error("--confirm needs a token");
                };
                confirm = Some(token);
            }
            flag if flag.starts_with('-') => {
                return usage_error(&format!("undo: unknown option {flag}"));
            }
            other => {
                return usage_error(&format!("undo: unexpected argument {other:?}"));
            }
        }
        index += 1;
    }
    let client = match MutationClient::from_env() {
        Ok(client) => client,
        Err(error) => return report_error(error),
    };
    let plan = match client.undo_plan(host) {
        Ok(plan) => plan,
        Err(error) => return report_undo_plan_error(error),
    };
    match client.undo_last(host, plan.action_id, confirm) {
        Ok(completed) => {
            println!(
                "dispatched: audit id {} (undo {} from action #{})",
                completed.action_id, plan.app_name, plan.action_id
            );
            ExitCode::SUCCESS
        }
        Err(error)
            if matches!(
                &error,
                ReadError::DaemonRequestFailed {
                    status: 403,
                    error: cause,
                    ..
                } if cause == "confirmation_required"
            ) =>
        {
            eprintln!(
                "zestroku: [{}] {error}; undo of {} requires approval; run \
                 `zestroku approve launch {host} {}` then retry with --confirm <token>",
                error.code(),
                plan.app_name,
                plan.app_id
            );
            ExitCode::from(4)
        }
        Err(error) => report_error(error),
    }
}

fn report_undo_plan_error(error: ReadError) -> ExitCode {
    let cannot_restore = matches!(
        &error,
        ReadError::DaemonRequestFailed {
            status: 404,
            error: cause,
            ..
        } if cause == "undo_app_not_installed"
    ) || matches!(
        &error,
        ReadError::DaemonRequestFailed {
            status: 409,
            error: cause,
            ..
        } if matches!(
            cause.as_str(),
            "undo_state_unavailable" | "undo_app_ambiguous" | "undo_app_not_launchable"
        )
    );
    if cannot_restore {
        eprintln!(
            "zestroku: [{}] could not restore the previous app: {error}",
            error.code()
        );
        ExitCode::from(5)
    } else {
        report_error(error)
    }
}

fn usage_error(error: &str) -> ExitCode {
    crate::report_cli_error(zestroku_types::ErrorCode::InvalidArgument, error, &[]);
    ExitCode::from(2)
}

fn report_error(error: ReadError) -> ExitCode {
    match &error {
        ReadError::DaemonRequestFailed {
            status: 403,
            error: cause,
            ..
        } if cause == "confirmation_required" => {
            eprintln!("zestroku: [{}] {error}", error.code());
            return ExitCode::from(4);
        }
        ReadError::DaemonRequestFailed {
            status: 403,
            error: cause,
            ..
        } if cause == "tier_refused" => {
            eprintln!("zestroku: [{}] {error}", error.code());
            return ExitCode::from(3);
        }
        _ => {}
    }
    let code = match &error {
        ReadError::InvalidDaemonUrl(_) | ReadError::InvalidDeviceTarget(_) => 2,
        ReadError::DaemonRequestFailed { status: 400, .. } => 2,
        ReadError::DaemonRequestFailed {
            status: 404, error, ..
        } if error == "device_not_found" => 2,
        ReadError::DaemonRequestFailed { status: 500, .. } => 1,
        _ => 5,
    };
    eprintln!("zestroku: [{}] {error}", error.code());
    ExitCode::from(code)
}
