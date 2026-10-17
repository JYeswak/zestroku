#![forbid(unsafe_code)]

use std::io::{self, Read};
use std::process::ExitCode;

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_core::mutation::{
    ClientPrincipal, MutationContext, MutationError, authorize_mutation, finish_mutation,
};
use zestroku_core::policy::Policy;
use zestroku_proto::{Command, Method, parse_active_app, query};
use zestroku_types::{AccessTier, Device, ErrorCode};

use crate::installer;
use crate::store_db;
use crate::transport::{AsupersyncTransport, send_selected};

const DEFAULT_INSTALLER_PORT: u16 = 80;
const PASSWORD_LIMIT: usize = 4096;

struct InstallOptions {
    host: String,
    archive: String,
    port: u16,
    username: String,
    confirm: Option<String>,
}

pub(crate) fn parse_port(raw: &str) -> Result<u16, String> {
    let port = raw
        .parse::<u16>()
        .map_err(|_| "--installer-port must be between 1 and 65535".to_string())?;
    if port == 0 {
        return Err("--installer-port must be between 1 and 65535".to_string());
    }
    Ok(port)
}

fn parse_options(args: &[String]) -> Result<InstallOptions, String> {
    let host = args.first().ok_or("install needs <host>")?.clone();
    let archive = args.get(1).ok_or("install needs <zip>")?.clone();
    let mut options = InstallOptions {
        host,
        archive,
        port: DEFAULT_INSTALLER_PORT,
        username: "rokudev".to_string(),
        confirm: None,
    };
    let mut password_stdin = false;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--installer-port" => {
                index += 1;
                options.port =
                    parse_port(args.get(index).ok_or("--installer-port needs a value")?)?;
            }
            "--username" => {
                index += 1;
                options.username = args.get(index).ok_or("--username needs a value")?.clone();
            }
            "--confirm" => {
                index += 1;
                options.confirm = Some(args.get(index).ok_or("--confirm needs a token")?.clone());
            }
            "--password-stdin" if !password_stdin => password_stdin = true,
            "--password-stdin" => return Err("--password-stdin may appear only once".to_string()),
            other => return Err(format!("unknown install option {other}")),
        }
        index += 1;
    }
    if !password_stdin {
        return Err(
            "install requires --password-stdin; passwords are never accepted as arguments"
                .to_string(),
        );
    }
    Ok(options)
}

fn read_password() -> Result<String, String> {
    let mut bytes = Vec::new();
    io::stdin()
        .take((PASSWORD_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "could not read Digest password from stdin".to_string())?;
    if bytes.len() > PASSWORD_LIMIT {
        return Err("Digest password input exceeds 4096 bytes".to_string());
    }
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    String::from_utf8(bytes).map_err(|_| "Digest password must be UTF-8".to_string())
}

fn runtime() -> Result<asupersync::runtime::Runtime, String> {
    let reactor = create_reactor().map_err(|error| error.to_string())?;
    RuntimeBuilder::current_thread()
        .with_reactor(reactor)
        .build()
        .map_err(|error| error.to_string())
}

pub fn run(args: &[String]) -> ExitCode {
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(error) => {
            crate::report_cli_error(ErrorCode::InvalidArgument, error, &[]);
            return ExitCode::from(2);
        }
    };
    // resolve_cli_target may rewrite <host> to a stored `ip:port` device id;
    // the installer dials options.port and the approval binds host + installer
    // port, so strip any resolved ECP suffix here (mirrors key/launch).
    let (dial_host, _) = crate::split_host_port(&options.host);
    let archive = match std::fs::read(&options.archive) {
        Ok(archive) => archive,
        Err(error) => {
            crate::report_cli_error(
                ErrorCode::InvalidArgument,
                format_args!("cannot read owner-provided ZIP: {error}"),
                &[],
            );
            return ExitCode::from(2);
        }
    };
    let filename = match std::path::Path::new(&options.archive)
        .file_name()
        .and_then(|name| name.to_str())
    {
        Some(filename) => filename.to_string(),
        None => {
            crate::report_cli_error(
                ErrorCode::InvalidArgument,
                "ZIP path has no valid filename",
                &[],
            );
            return ExitCode::from(2);
        }
    };
    let command = Command {
        method: Method::Post,
        path: "/plugin_install".to_string(),
        tier: AccessTier::Permissive,
    };
    let store = match store_db::open() {
        Ok(store) => store,
        Err(error) => {
            crate::report_cli_error(ErrorCode::StoreUnavailable, error, &[]);
            return ExitCode::FAILURE;
        }
    };
    let recorded = match zestroku_core::tier::recorded_tier_for(&store, &dial_host, 8060) {
        Ok(recorded) => recorded,
        Err(error) => {
            crate::report_cli_error(ErrorCode::StoreUnavailable, error, &[]);
            return ExitCode::FAILURE;
        }
    };
    let (tier_source, access_tier) = match std::env::var("ZESTROKU_TIER").ok().as_deref() {
        Some("permissive") | Some("Permissive") => ("env-override", AccessTier::Permissive),
        Some(other) if !other.trim().is_empty() && !other.eq_ignore_ascii_case("open") => {
            match recorded {
                Some(tier) => ("recorded", tier.access_tier()),
                None => {
                    crate::report_cli_error(
                        ErrorCode::TierRefused,
                        format_args!("no recorded tier for {dial_host}; run zestroku info first"),
                        &[],
                    );
                    return ExitCode::from(3);
                }
            }
        }
        _ => match recorded {
            Some(tier) => ("recorded", tier.access_tier()),
            None => {
                crate::report_cli_error(
                    ErrorCode::TierRefused,
                    format_args!("no recorded tier for {dial_host}; run zestroku info first"),
                    &[],
                );
                return ExitCode::from(3);
            }
        },
    };
    let device = Device {
        host: dial_host.clone(),
        port: options.port,
        model_name: String::new(),
        model_number: String::new(),
        software_version: String::new(),
        is_tv: false,
        access_tier,
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            crate::report_cli_error(ErrorCode::RuntimeUnavailable, error, &[]);
            return ExitCode::FAILURE;
        }
    };
    let before = runtime.block_on(async {
        match send_selected(AsupersyncTransport, &dial_host, 8060, &query("active-app")).await {
            Ok(response) => Some(
                parse_active_app(&response.body)
                    .map(|app| (app.app_id, app.name))
                    .unwrap_or((None, "unparseable".to_string())),
            ),
            Err(_) => None,
        }
    });
    let marked_before = match &before {
        Some((app_id, name)) => zestroku_core::tier::marked_before_app_state(
            tier_source,
            &format!("{dial_host}:8060"),
            *app_id,
            name,
        ),
        // Unreachable probe: name-only legacy shape that the undo selector
        // intentionally refuses (no restore identity).
        None => zestroku_core::tier::marked_before_state(tier_source, "unreachable"),
    };
    let context = MutationContext {
        client: ClientPrincipal::Cli.as_str(),
        surface: "install",
        intent: "install developer sideload ZIP",
        before_state: &marked_before,
    };
    let policy = match Policy::load_default() {
        Ok(policy) => policy,
        Err(error) => {
            crate::report_cli_error(ErrorCode::ConfigurationError, error, &[]);
            return ExitCode::FAILURE;
        }
    };
    let action_id = match authorize_mutation(
        &store,
        &device,
        &command,
        context,
        options.confirm.as_deref(),
        &policy,
    ) {
        Ok(action_id) => action_id,
        Err(MutationError::ConfirmationRequired { action_id, .. }) => {
            crate::report_cli_error(
                ErrorCode::ConfirmationRequired,
                format_args!(
                    "run zestroku approve install {dial_host} sideload --installer-port {} then retry with --confirm <token> (audit id {action_id})",
                    options.port
                ),
                &[],
            );
            return ExitCode::from(4);
        }
        Err(MutationError::PolicyDenied { action_id }) => {
            crate::report_cli_error(
                ErrorCode::PolicyDenied,
                format_args!("denial audited, id {action_id}"),
                &[],
            );
            return ExitCode::from(5);
        }
        Err(MutationError::TierRefused(error)) => {
            crate::report_cli_error(error.code(), format_args!("{error}; denial audited"), &[]);
            return ExitCode::from(3);
        }
        Err(error) => {
            crate::report_cli_error(crate::mutate::mutation_error_code(&error), &error, &[]);
            return ExitCode::FAILURE;
        }
    };
    let result = read_password()
        .map_err(|error| (ErrorCode::InvalidArgument, error))
        .and_then(|password| {
            installer::install(
                &dial_host,
                options.port,
                &archive,
                &filename,
                &options.username,
                &password,
            )
            .map_err(|error| (ErrorCode::InstallFailed, error))
        });
    let outcome = match &result {
        Ok(()) => "installer reported success".to_string(),
        Err((code, error)) => format!("installer failed: {}: {error}", code.as_str()),
    };
    if let Err(error) = finish_mutation(&store, action_id, &outcome) {
        eprintln!("zestroku: warning: could not record install outcome: {error}");
    }
    match result {
        Ok(()) => {
            println!("ok: sideloaded owner ZIP (audit id {action_id})");
            ExitCode::SUCCESS
        }
        Err((code, error)) => {
            crate::report_cli_error(code, error, &[]);
            ExitCode::from(5)
        }
    }
}
