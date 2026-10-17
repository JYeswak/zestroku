//! `zestroku doctor [--json]`: run the core check registry against the local
//! store with live loopback/LAN probes. Read-only: never mutates device
//! state. Exit 0 all pass (skips allowed), 6 any warning, 7 any failure.
#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::time::Duration;

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_core::doctor::{self, ConnectOutcome, Ctx, Verdict};
use zestroku_types::ErrorCode;

use crate::inventory;
use crate::store_db;
use crate::transport::StdFallbackTransport;

fn connect_probe(host: &str, port: u16) -> ConnectOutcome {
    let addr: std::net::SocketAddr = match format!("{host}:{port}").parse() {
        Ok(addr) => addr,
        Err(_) => return ConnectOutcome::Timeout,
    };
    match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(_) => ConnectOutcome::Ok,
        Err(error) => match error.kind() {
            std::io::ErrorKind::HostUnreachable => ConnectOutcome::HostUnreachable,
            std::io::ErrorKind::ConnectionRefused => ConnectOutcome::Refused,
            std::io::ErrorKind::TimedOut => ConnectOutcome::Timeout,
            _ => ConnectOutcome::Timeout,
        },
    }
}

/// Platform-tool control for `lan-policy` (bead amendment): a macOS-native
/// `nc` probe answers whether the OS itself can reach the host.
fn platform_ok(host: &str) -> bool {
    std::process::Command::new("/usr/bin/nc")
        .args(["-z", "-G", "2", host, "8060"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// JSON shape (documented): an array of
/// `{id, verdict, detail, evidence, remedy}` with verdict one of
/// `pass|warn|fail|skip`.
fn verdict_str(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "pass",
        Verdict::Warn => "warn",
        Verdict::Fail => "fail",
        Verdict::Skip => "skip",
    }
}

pub fn run(args: &[String]) -> ExitCode {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            _ => {
                crate::report_cli_error(
                    ErrorCode::InvalidArgument,
                    "usage: zestroku doctor [--json]",
                    &[],
                );
                return ExitCode::from(2);
            }
        }
    }
    let store = match store_db::open() {
        Ok(store) => store,
        Err(error) => {
            crate::report_cli_error(
                ErrorCode::StoreUnavailable,
                format_args!("doctor store: {error}"),
                &[],
            );
            return ExitCode::FAILURE;
        }
    };
    let db_path = store_db::db_path();
    let seeds = match inventory::read_seeds(None) {
        Ok(seeds) => seeds,
        Err(error) => {
            crate::report_cli_error(
                ErrorCode::ConfigurationError,
                format_args!("doctor targets: {error}"),
                &[],
            );
            return ExitCode::FAILURE;
        }
    };
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
            crate::report_cli_error(
                ErrorCode::RuntimeUnavailable,
                format_args!("doctor could not start runtime: {error}"),
                &[],
            );
            return ExitCode::FAILURE;
        }
    };
    let transport = StdFallbackTransport;
    let ctx = Ctx {
        store: &store,
        store_path: &db_path,
        targets_toml: seeds.as_deref(),
        transport: &transport,
        ssdp_reply: None,
        discovered_ids: None,
        connect: connect_probe,
        platform_ok,
    };
    let findings = runtime.block_on(doctor::run_checked(&ctx, &[]));
    if json {
        let documents: Vec<serde_json::Value> = findings
            .iter()
            .map(|finding| {
                serde_json::json!({
                    "id": finding.id,
                    "verdict": verdict_str(finding.verdict),
                    "detail": finding.detail,
                    "evidence": finding.evidence,
                    "remedy": finding.remedy,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&documents).unwrap_or_else(|_| "[]".to_string())
        );
    } else {
        println!("{:<20} {:<6} DETAIL", "CHECK", "STATUS");
        for finding in &findings {
            println!(
                "{:<20} {:<6} {}",
                finding.id,
                verdict_str(finding.verdict),
                finding.detail
            );
        }
        for finding in findings.iter().filter(|f| {
            f.verdict == Verdict::Fail || (f.verdict == Verdict::Warn && !f.remedy.is_empty())
        }) {
            println!("remedy [{}]: {}", finding.id, finding.remedy);
        }
    }
    if findings.iter().any(|f| f.verdict == Verdict::Fail) {
        crate::report_cli_error(ErrorCode::DoctorFail, "doctor found failing checks", &[]);
        ExitCode::from(7)
    } else if findings.iter().any(|f| f.verdict == Verdict::Warn) {
        crate::report_cli_error(ErrorCode::DoctorWarn, "doctor found warnings", &[]);
        ExitCode::from(6)
    } else {
        ExitCode::SUCCESS
    }
}
