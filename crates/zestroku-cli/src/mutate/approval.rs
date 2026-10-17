//! Owner-only approval issuance. Error rendering must never bypass the TTY or phrase checks.

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use zestroku_core::mutation::{ClientPrincipal, issue_approval};
use zestroku_types::{AccessTier, Device, ErrorCode};

use super::{build_command, mutation_error_code};

fn refusal(code: ErrorCode, detail: impl std::fmt::Display) -> ExitCode {
    crate::report_cli_error(code, detail, &[]);
    ExitCode::from(2)
}

pub(crate) fn run(args: &[String]) -> ExitCode {
    let Some(verb) = args.first().map(String::as_str) else {
        return refusal(
            ErrorCode::InvalidArgument,
            "usage: zestroku approve <launch|key|install> <host> <target> [--client <cli|mcp-stdio|loopback-http>] [--installer-port <port>]",
        );
    };
    let install = verb == "install";
    if (!install && !matches!(verb, "launch" | "key")) || args.len() < 3 {
        return refusal(
            ErrorCode::InvalidArgument,
            "usage: zestroku approve <launch|key> <host> <target> [--client <cli|mcp-stdio|loopback-http>] | approve install <host> sideload [--installer-port <port>] [--client <cli|mcp-stdio|loopback-http>]",
        );
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return refusal(
            ErrorCode::ConfirmationRequired,
            "zestroku approve requires an interactive terminal",
        );
    }
    let host_label = &args[1];
    let target = &args[2];
    let mut client = ClientPrincipal::Cli;
    let mut client_seen = false;
    let mut installer_port = 80;
    let mut installer_port_seen = false;
    let mut index = 3;
    while index < args.len() {
        match args[index].as_str() {
            "--client" => {
                if client_seen {
                    return refusal(ErrorCode::InvalidArgument, "approve takes one --client");
                }
                index += 1;
                let Some(value) = args.get(index) else {
                    return refusal(
                        ErrorCode::InvalidArgument,
                        "--client needs cli, mcp-stdio, or loopback-http",
                    );
                };
                let Some(parsed) = ClientPrincipal::parse(value) else {
                    return refusal(
                        ErrorCode::InvalidArgument,
                        format_args!("unsupported approval client {value:?}"),
                    );
                };
                client = parsed;
                client_seen = true;
            }
            "--installer-port" if install => {
                if installer_port_seen {
                    return refusal(
                        ErrorCode::InvalidArgument,
                        "approve install takes one --installer-port",
                    );
                }
                index += 1;
                let Some(value) = args.get(index) else {
                    return refusal(ErrorCode::InvalidArgument, "--installer-port needs a port");
                };
                installer_port = match crate::install::parse_port(value) {
                    Ok(port) => port,
                    Err(error) => return refusal(ErrorCode::InvalidArgument, error),
                };
                installer_port_seen = true;
            }
            "--installer-port" => {
                return refusal(
                    ErrorCode::InvalidArgument,
                    "--installer-port needs install approval",
                );
            }
            other => {
                return refusal(
                    ErrorCode::InvalidArgument,
                    format_args!("unknown approval option {other}"),
                );
            }
        }
        index += 1;
    }
    if install && target != "sideload" {
        return refusal(
            ErrorCode::InvalidArgument,
            "install approval target must be sideload",
        );
    }
    let (host, port) = if install {
        (host_label.clone(), installer_port)
    } else {
        crate::split_host_port(host_label)
    };
    let command = match build_command(verb, target) {
        Ok(command) => command,
        Err(error) => return refusal(ErrorCode::InvalidArgument, error),
    };
    let phrase = if install {
        format!(
            "approve install {host_label} sideload --installer-port {port} --client {}",
            client.as_str()
        )
    } else {
        format!(
            "approve {verb} {host_label} {target} --client {}",
            client.as_str()
        )
    };
    eprint!("Type exactly '{phrase}' to authorize this action: ");
    let _ = io::stderr().flush();
    let mut typed = String::new();
    if io::stdin().read_line(&mut typed).is_err() || typed.trim_end_matches(['\r', '\n']) != phrase
    {
        return refusal(
            ErrorCode::ConfirmationRequired,
            "approval phrase did not match",
        );
    }
    let store = match crate::store_db::open() {
        Ok(store) => store,
        Err(error) => {
            crate::report_cli_error(ErrorCode::StoreUnavailable, error, &[]);
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
    match issue_approval(&store, &device, &command, client, now) {
        Ok(token) => {
            println!("{token}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            crate::report_cli_error(mutation_error_code(&error), &error, &[]);
            ExitCode::FAILURE
        }
    }
}
