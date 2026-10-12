#![forbid(unsafe_code)]

use std::io::{self, Read};
use std::process::ExitCode;

use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use zestroku_core::mutation::{
    MutationContext, MutationError, authorize_mutation, finish_mutation,
};
use zestroku_proto::{Command, Method, parse_active_app, query};
use zestroku_types::{AccessTier, Device};

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
            eprintln!("zestroku: {error}");
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
            eprintln!("zestroku: cannot read owner-provided ZIP: {error}");
            return ExitCode::from(2);
        }
    };
    let filename = match std::path::Path::new(&options.archive)
        .file_name()
        .and_then(|name| name.to_str())
    {
        Some(filename) => filename.to_string(),
        None => {
            eprintln!("zestroku: ZIP path has no valid filename");
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
            eprintln!("zestroku: action log unavailable: {error}");
            return ExitCode::FAILURE;
        }
    };
    let recorded = match zestroku_core::tier::recorded_tier_for(&store, &dial_host, 8060) {
        Ok(recorded) => recorded,
        Err(error) => {
            eprintln!("zestroku: cannot read recorded ECP tier: {error}");
            return ExitCode::FAILURE;
        }
    };
    let (tier_source, access_tier) = match std::env::var("ZESTROKU_TIER").ok().as_deref() {
        Some("permissive") | Some("Permissive") => ("env-override", AccessTier::Permissive),
        Some(other) if !other.trim().is_empty() && !other.eq_ignore_ascii_case("open") => {
            match recorded {
                Some(tier) => ("recorded", tier.access_tier()),
                None => {
                    eprintln!(
                        "zestroku: no recorded tier for {} — run zestroku info first or set ZESTROKU_TIER=permissive",
                        dial_host
                    );
                    return ExitCode::from(3);
                }
            }
        }
        _ => match recorded {
            Some(tier) => ("recorded", tier.access_tier()),
            None => {
                eprintln!(
                    "zestroku: no recorded tier for {} — run zestroku info first or set ZESTROKU_TIER=permissive",
                    dial_host
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
            eprintln!("zestroku: could not start asupersync runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let before = runtime.block_on(async {
        match send_selected(
            AsupersyncTransport,
            &dial_host,
            8060,
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
    let marked_before = zestroku_core::tier::marked_before_state(tier_source, &before);
    let context = MutationContext {
        client: "cli",
        surface: "install",
        intent: "install developer sideload ZIP",
        before_state: &marked_before,
    };
    let action_id = match authorize_mutation(
        &store,
        &device,
        &command,
        context,
        options.confirm.as_deref(),
    ) {
        Ok(action_id) => action_id,
        Err(MutationError::ConfirmationRequired { action_id, .. }) => {
            eprintln!(
                "zestroku: CONFIRMATION_REQUIRED — run zestroku approve install {} sideload --installer-port {} then retry with --confirm <token> (audit id {action_id})",
                dial_host, options.port
            );
            return ExitCode::from(4);
        }
        Err(MutationError::TierRefused(error)) => {
            eprintln!("zestroku: {error} (denial audited)");
            return ExitCode::from(3);
        }
        Err(error) => {
            eprintln!("zestroku: mutation gate failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = read_password().and_then(|password| {
        installer::install(
            &dial_host,
            options.port,
            &archive,
            &filename,
            &options.username,
            &password,
        )
    });
    let outcome = match &result {
        Ok(()) => "installer reported success".to_string(),
        Err(error) => format!("installer failed: {error}"),
    };
    if let Err(error) = finish_mutation(&store, action_id, &outcome) {
        eprintln!("zestroku: warning: could not record install outcome: {error}");
    }
    match result {
        Ok(()) => {
            println!("ok: sideloaded owner ZIP (audit id {action_id})");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("zestroku: {error}");
            ExitCode::from(5)
        }
    }
}
