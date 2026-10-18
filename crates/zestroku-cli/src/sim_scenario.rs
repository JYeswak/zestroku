//! `zestroku sim --scenario two-devices`: serve one Express and one TV
//! Permissive simulator on loopback, walk the read workflow
//! (device-info, apps, active-app, tv-active-channel) on each, and print a
//! JSONL trace. Read-only: only GETs, no store writes, no mutations.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use zestroku_proto::{Transport, query};
use zestroku_types::ErrorCode;

use crate::transport::SelectedTransport;

struct Step {
    device: &'static str,
    route: &'static str,
    status: u16,
    ok: bool,
    detail: String,
}

async fn get(port: u16, route: &str) -> (u16, String) {
    match SelectedTransport
        .send("127.0.0.1", port, &query(route))
        .await
    {
        Ok(response) => (response.status, response.body),
        Err(error) => (0, format!("transport error: {error}")),
    }
}

async fn walk_one(label: &'static str, port: u16, is_tv: bool, trace: &mut Vec<Step>) -> bool {
    let mut ok = true;
    for route in ["device-info", "apps", "active-app", "tv-active-channel"] {
        let (status, body) = get(port, route).await;
        let expected = match route {
            "tv-active-channel" if !is_tv => 404,
            _ => 200,
        };
        let mut step_ok = status == expected;
        let mut detail = format!("status={status} expected={expected}");
        if step_ok && route == "device-info" && !body.contains("<model-number>") {
            step_ok = false;
            detail.push_str("; device-info lacks model-number");
        }
        if step_ok && route == "apps" && !body.contains("<app ") {
            step_ok = false;
            detail.push_str("; apps lacks app rows");
        }
        if step_ok && route == "tv-active-channel" && is_tv && body.is_empty() {
            step_ok = false;
            detail.push_str("; empty tv channel body");
        }
        ok &= step_ok;
        trace.push(Step {
            device: label,
            route,
            status,
            ok: step_ok,
            detail,
        });
    }
    ok
}

fn runtime() -> Result<asupersync::runtime::Runtime, String> {
    use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
    let reactor = create_reactor().map_err(|error| error.to_string())?;
    RuntimeBuilder::current_thread()
        .with_reactor(reactor)
        .build()
        .map_err(|error| error.to_string())
}

/// Run `zestroku sim --scenario two-devices [--json]`.
pub fn run(args: &[String]) -> ExitCode {
    let mut scenario = None;
    let mut json = false;
    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--scenario" => {
                scenario = it.next().map(String::as_str);
            }
            "--json" => json = true,
            other => {
                crate::report_cli_error(
                    ErrorCode::InvalidArgument,
                    format_args!("sim: unknown option {other}"),
                    &[],
                );
                return ExitCode::from(2);
            }
        }
    }
    let Some("two-devices") = scenario else {
        crate::report_cli_error(
            ErrorCode::InvalidArgument,
            "sim: usage: sim --scenario two-devices [--json]",
            &[],
        );
        return ExitCode::from(2);
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            crate::report_cli_error(
                ErrorCode::RuntimeUnavailable,
                format_args!("sim: could not start runtime: {error}"),
                &[],
            );
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let express = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
            kind: zestroku_sim::DeviceKind::Express,
            tier: zestroku_sim::SimTier::Permissive,
        });
        let tv = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
            kind: zestroku_sim::DeviceKind::Tv,
            tier: zestroku_sim::SimTier::Permissive,
        });
        let mut trace = Vec::new();
        let mut ok = true;
        ok &= walk_one("express", express.port(), false, &mut trace).await;
        ok &= walk_one("tv", tv.port(), true, &mut trace).await;
        for step in &trace {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "device": step.device,
                        "route": step.route,
                        "status": step.status,
                        "ok": step.ok,
                        "detail": step.detail,
                    })
                );
            } else {
                println!(
                    "{} {} -> {}{}",
                    step.device,
                    step.route,
                    step.status,
                    if step.ok { "" } else { " MISMATCH" }
                );
            }
        }
        if ok {
            ExitCode::SUCCESS
        } else {
            crate::report_cli_error(
                ErrorCode::ProtocolError,
                "sim: two-devices scenario failed",
                &[],
            );
            ExitCode::FAILURE
        }
    })
}
