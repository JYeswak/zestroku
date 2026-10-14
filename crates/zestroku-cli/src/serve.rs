//! `zestroku serve [--bind <ip:port>] [--allow-unsafe-bind]`
//!
//! The read-only HTTP API (`zestroku-api::build_app`: `/health`,
//! `/v1/devices`, `/v1/devices/{host}/info`, `/v1/devices/{host}/active-app`)
//! over the shared store and the ECP transport, on one asupersync runtime.
//! The listener binds loopback or a private-LAN address only (wildcard needs
//! `--allow-unsafe-bind`), and validates the Host header against the bind IP.
//! The daemon, not the agent's process tree, is what talks to the devices.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use asupersync::combinator::{Either, Select};
use asupersync::net::TcpListener;
use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use asupersync::signal::{SignalKind, signal};
use fastapi::{ServerConfig, TcpServer};

use crate::store_db;
use crate::transport::AsupersyncTransport;

const DEFAULT_BIND: &str = "127.0.0.1:8070";

struct Opts {
    bind: String,
    allow_wildcard: bool,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        bind: DEFAULT_BIND.to_string(),
        allow_wildcard: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bind" => o.bind = it.next().ok_or("--bind needs ip:port")?.clone(),
            "--allow-unsafe-bind" => o.allow_wildcard = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

async fn serve_until_shutdown(
    app: fastapi::App,
    bind_addr: String,
    config: ServerConfig,
) -> Result<(), String> {
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|error| format!("could not register SIGTERM handler: {error}"))?;
    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|error| format!("could not register SIGINT handler: {error}"))?;
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
        Ok(o) => o,
        Err(e) => {
            eprintln!("zestroku serve: {e}");
            return ExitCode::from(2);
        }
    };
    let addr = match zestroku_api::check_bind(&opts.bind, opts.allow_wildcard) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("zestroku serve: {e}");
            return ExitCode::from(2);
        }
    };
    let store = match store_db::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("zestroku serve: store unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let app = zestroku_api::build_app(Arc::new(Mutex::new(store)), Arc::new(AsupersyncTransport));
    let bind_addr = addr.to_string();
    let mut config = ServerConfig::new(bind_addr.clone());
    if !addr.ip().is_unspecified() {
        config = config.with_allowed_hosts([addr.ip().to_string()]);
    }
    let runtime = match create_reactor().map_err(|e| e.to_string()).and_then(|r| {
        RuntimeBuilder::current_thread()
            .with_reactor(r)
            .build()
            .map_err(|e| e.to_string())
    }) {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("zestroku serve: could not start asupersync runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(serve_until_shutdown(app, bind_addr, config));
    if !runtime.shutdown_timeout(Duration::from_secs(30)) {
        eprintln!("zestroku serve: asupersync runtime did not drain within 30s");
        return ExitCode::FAILURE;
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zestroku serve: {e}");
            ExitCode::FAILURE
        }
    }
}
