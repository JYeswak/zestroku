//! `zestroku serve [--bind <ip:port>]`
//!
//! The read-only HTTP API (`zestroku-api::build_app`: `/health`,
//! `/v1/devices`, `/v1/devices/{host}/info`, `/v1/devices/{host}/active-app`)
//! over the shared store and ECP transport, on one asupersync runtime.
//! The listener binds loopback or one of this node's Tailscale addresses
//! only, and validates the Host header against the bind IP.
//! The daemon, not the agent's process tree, talks to the devices.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use asupersync::combinator::{Either, Select};
use asupersync::net::TcpListener;
use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use asupersync::signal::{SignalKind, signal};
use fastapi::{ServerConfig, TcpServer};
use zestroku_core::store::SqliteStore;
use zestroku_core::tier::{classify_tier, device_id, record_probe};
use zestroku_proto::{Transport, parse_ecp_response, query};

use crate::inventory;
use crate::store_db;
use crate::transport::SelectedTransport;

const DEFAULT_BIND: &str = "127.0.0.1:8070";

struct Opts {
    bind: String,
    targets: Option<PathBuf>,
    no_ssdp: bool,
    mx: u8,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts {
        bind: DEFAULT_BIND.to_string(),
        targets: std::env::var_os("ZESTROKU_TARGETS").map(PathBuf::from),
        no_ssdp: false,
        mx: 3,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bind" => opts.bind = it.next().ok_or("--bind needs ip:port")?.clone(),
            "--targets" => {
                opts.targets = Some(PathBuf::from(it.next().ok_or("--targets needs a file")?));
            }
            "--no-ssdp" => opts.no_ssdp = true,
            "--mx" => {
                opts.mx = it
                    .next()
                    .and_then(|value| value.parse::<u8>().ok())
                    .filter(|value| (1..=5).contains(value))
                    .ok_or("--mx needs 1..=5")?;
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(opts)
}

async fn discover_on_startup(
    store: &Arc<Mutex<SqliteStore>>,
    transport: &Arc<SelectedTransport>,
    opts: &Opts,
) -> Result<(), String> {
    let seeds = inventory::read_seeds(opts.targets.as_ref())?;
    if opts.no_ssdp && seeds.is_none() {
        return Err("--no-ssdp needs a targets.toml (--targets or ZESTROKU_TARGETS)".into());
    }
    let (devices, mut notes) = if opts.no_ssdp {
        inventory::seeds_only(seeds.as_deref().unwrap_or_default())
            .await
            .map_err(|error| format!("seed discovery failed: {error}"))?
    } else {
        zestroku_core::ssdp_io::discover(transport.as_ref(), seeds.as_deref(), opts.mx)
            .await
            .map_err(|error| format!("SSDP discovery failed: {error}"))?
    };

    let persistence_error = {
        let store = store
            .lock()
            .map_err(|_| "inventory store lock poisoned".to_string())?;
        inventory::persist_inventory(Some(&store), &devices)
    };
    if let Some(error) = persistence_error {
        return Err(format!("inventory persistence failed: {error}"));
    }

    for (device, _) in &devices {
        let response = match transport
            .send(&device.host, device.port, &query("apps"))
            .await
        {
            Ok(response) => response,
            Err(error) => {
                notes.push(format!(
                    "{}:{}: apps tier probe failed: {error}",
                    device.host, device.port
                ));
                continue;
            }
        };
        let gate = parse_ecp_response(&response).gate;
        let observed = classify_tier(gate);
        let id = device_id(&device.host, device.port);
        let store = store
            .lock()
            .map_err(|_| "inventory store lock poisoned".to_string())?;
        record_probe(&*store, &id, observed, &format!("{}", gate as i32)).map_err(|error| {
            format!(
                "tier persistence failed for {}:{}: {error}",
                device.host, device.port
            )
        })?;
    }
    for note in notes {
        eprintln!("zestroku serve: startup discovery note: {note}");
    }
    Ok(())
}

async fn serve_until_shutdown(
    app: fastapi::App,
    bind_addr: String,
    config: ServerConfig,
    store: Arc<Mutex<SqliteStore>>,
    transport: Arc<SelectedTransport>,
    opts: Opts,
) -> Result<(), String> {
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|error| format!("could not register SIGTERM handler: {error}"))?;
    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|error| format!("could not register SIGINT handler: {error}"))?;

    discover_on_startup(&store, &transport, &opts).await?;

    match app.run_startup_hooks().await {
        fastapi::fastapi_core::StartupOutcome::Success => {}
        fastapi::fastapi_core::StartupOutcome::PartialSuccess { warnings } => {
            eprintln!("Warning: {warnings} startup hook(s) had non-fatal errors");
        }
        fastapi::fastapi_core::StartupOutcome::Aborted(error) => {
            return Err(format!("startup failed: {error}"));
        }
    }

    let app = Arc::new(app);
    let cx = fastapi::Cx::current()
        .ok_or_else(|| "fastapi App::serve must run inside an asupersync runtime".to_string())?;
    let listener = match TcpListener::bind(bind_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            app.run_shutdown_hooks().await;
            return Err(format!("HTTP server failed: {error}"));
        }
    };
    let bound_addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(error) => {
            app.run_shutdown_hooks().await;
            return Err(format!("HTTP server failed: {error}"));
        }
    };

    eprintln!("zestroku serve: listening on http://{bound_addr}/");
    let server = TcpServer::new(config);
    let mut serving =
        std::pin::pin!(server.serve_on_app_concurrent(&cx, listener, Arc::clone(&app)));
    let mut sigterm_recv = std::pin::pin!(sigterm.recv());
    let mut sigint_recv = std::pin::pin!(sigint.recv());
    let mut receiving_signal =
        std::pin::pin!(Select::new(sigterm_recv.as_mut(), sigint_recv.as_mut()));

    let result = match Select::new(serving.as_mut(), receiving_signal.as_mut()).await {
        Ok(Either::Left(result)) => result.map_err(|error| format!("HTTP server failed: {error}")),
        Ok(Either::Right(Ok(_))) => {
            server.shutdown();
            serving
                .as_mut()
                .await
                .map_err(|error| format!("HTTP server drain failed: {error}"))
        }
        Ok(Either::Right(Err(error))) => Err(format!("signal selection failed: {error:?}")),
        Err(error) => Err(format!("shutdown selection failed: {error:?}")),
    };
    app.run_shutdown_hooks().await;
    result
}

pub fn run(args: &[String]) -> ExitCode {
    let opts = match parse_opts(args) {
        Ok(opts) => opts,
        Err(error) => {
            eprintln!("zestroku serve: {error}");
            return ExitCode::from(2);
        }
    };
    let addr = match zestroku_api::check_bind(&opts.bind) {
        Ok(addr) => addr,
        Err(error) => {
            eprintln!("zestroku serve: {error}");
            return ExitCode::from(2);
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
            eprintln!("zestroku serve: could not start asupersync runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !addr.ip().is_loopback() {
        let owned = match runtime.block_on(zestroku_api::tailscale_self_ips()) {
            Ok(ips) => zestroku_api::node_owns_address(&addr.ip(), &ips),
            Err(error) => {
                eprintln!("zestroku serve: tailnet bind refused ({error})");
                return ExitCode::from(2);
            }
        };
        if !owned {
            eprintln!(
                "zestroku serve: tailnet bind refused ({} is not this node address)",
                addr.ip()
            );
            return ExitCode::from(2);
        }
    }
    let store = match store_db::open() {
        Ok(store) => Arc::new(Mutex::new(store)),
        Err(error) => {
            eprintln!("zestroku serve: store unavailable: {error}");
            return ExitCode::FAILURE;
        }
    };
    let transport = Arc::new(SelectedTransport);
    let app = zestroku_api::build_app(Arc::clone(&store), Arc::clone(&transport));
    let bind_addr = addr.to_string();
    let mut config = ServerConfig::new(bind_addr.clone());
    if !addr.ip().is_unspecified() {
        config = config.with_allowed_hosts([addr.ip().to_string()]);
    }
    let result = runtime.block_on(serve_until_shutdown(
        app, bind_addr, config, store, transport, opts,
    ));
    if !runtime.shutdown_timeout(Duration::from_secs(30)) {
        eprintln!("zestroku serve: asupersync runtime did not drain within 30s");
        return ExitCode::FAILURE;
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zestroku serve: {error}");
            ExitCode::FAILURE
        }
    }
}
