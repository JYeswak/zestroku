//! `zestroku serve [--bind <ip:port>]`
//!
//! The HTTP API's reads (`/health`, `/v1/devices`, `info`, `active-app`,
//! `apps`, and `state`) and per-operation-approved mutations share the store
//! and ECP transport on one asupersync runtime. A structured per-device
//! poller feeds the fresh state cache; the API and MCP consumers never open
//! direct ECP connections.
//! The listener binds loopback or one of this node's Tailscale addresses
//! only, and validates the Host header against the bind IP.
//! The daemon, not the agent's process tree, talks to the devices.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::inventory;
use crate::store_db;
use crate::transport::SelectedTransport;
use asupersync::combinator::{Either, Select};
use asupersync::net::TcpListener;
use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
use asupersync::signal::{SignalKind, signal};
use fastapi::{ServerConfig, TcpServer};
use zestroku_core::store::{DeviceRow, SqliteStore, Store};
use zestroku_core::tier::{classify_tier, classify_tier_from_response, device_id, record_probe};
use zestroku_core::{AppsSnapshot, DeviceStateSnapshot, Inventory, MediaPlayerSnapshot};
use zestroku_proto::{
    Command, Transport, parse_active_app, parse_apps, parse_device_info, parse_ecp_response,
    parse_media_player, parse_tv_active_channel, query, query_apps,
};
use zestroku_types::Device;

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

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn row_port(row: &DeviceRow) -> Option<u16> {
    let (_, port) = row.id.rsplit_once(':')?;
    port.parse::<u16>().ok().filter(|port| *port != 0)
}

async fn fetch_ecp(
    transport: &SelectedTransport,
    host: &str,
    port: u16,
    command: &Command,
) -> std::result::Result<zestroku_proto::EcpResponse, String> {
    transport
        .send(host, port, command)
        .await
        .map_err(|error| error.to_string())
}

async fn poll_device_state(
    store: &Arc<Mutex<SqliteStore>>,
    transport: &SelectedTransport,
    host: &str,
    port: u16,
    target_id: &str,
) -> std::result::Result<DeviceStateSnapshot, String> {
    let info_response = fetch_ecp(transport, host, port, &query("device-info")).await?;
    if parse_ecp_response(&info_response).gate != zestroku_proto::EcpGate::Open {
        return Err(format!(
            "device-info returned HTTP {}",
            info_response.status
        ));
    }
    let info = parse_device_info(&info_response.body).map_err(|error| error.to_string())?;

    let apps_response = fetch_ecp(transport, host, port, &query_apps()).await?;
    let apps_gate = parse_ecp_response(&apps_response).gate;
    let observed_tier = classify_tier_from_response(&apps_response);
    let probe_outcome = format!("{}", apps_gate as i32);
    store
        .lock()
        .map_err(|_| "inventory store lock poisoned".to_string())
        .and_then(|store| {
            record_probe(&*store, target_id, observed_tier, &probe_outcome)
                .map_err(|error| error.to_string())
        })?;
    let apps = match apps_gate {
        zestroku_proto::EcpGate::Open => AppsSnapshot::Available(
            parse_apps(&apps_response.body).map_err(|error| error.to_string())?,
        ),
        zestroku_proto::EcpGate::Limited403 => AppsSnapshot::TierRefused,
        zestroku_proto::EcpGate::Absent404 => AppsSnapshot::EndpointAbsent,
        zestroku_proto::EcpGate::Unknown403 => AppsSnapshot::UnknownForbidden,
        zestroku_proto::EcpGate::Other => {
            return Err(format!("query/apps returned HTTP {}", apps_response.status));
        }
    };

    let active_response = fetch_ecp(transport, host, port, &query("active-app")).await?;
    if parse_ecp_response(&active_response).gate != zestroku_proto::EcpGate::Open {
        return Err(format!(
            "query/active-app returned HTTP {}",
            active_response.status
        ));
    }
    let active_app = parse_active_app(&active_response.body).map_err(|error| error.to_string())?;

    let media_response = fetch_ecp(transport, host, port, &query("media-player")).await?;
    let media_player = match parse_ecp_response(&media_response).gate {
        zestroku_proto::EcpGate::Open => MediaPlayerSnapshot::Available(
            parse_media_player(&media_response).map_err(|error| error.to_string())?,
        ),
        zestroku_proto::EcpGate::Limited403 => MediaPlayerSnapshot::TierRefused,
        zestroku_proto::EcpGate::Absent404 => MediaPlayerSnapshot::EndpointAbsent,
        zestroku_proto::EcpGate::Unknown403 => MediaPlayerSnapshot::UnknownForbidden,
        zestroku_proto::EcpGate::Other => {
            return Err(format!(
                "query/media-player returned HTTP {}",
                media_response.status
            ));
        }
    };

    let channel_response = fetch_ecp(transport, host, port, &query("tv-active-channel")).await?;
    let tv_active_channel = parse_tv_active_channel(&channel_response).ok();
    let device = Device {
        host: host.to_owned(),
        port,
        model_name: info.model_name,
        model_number: info.model_number,
        software_version: info.software_version,
        is_tv: info.is_tv,
        access_tier: observed_tier.access_tier(),
    };
    Ok(DeviceStateSnapshot {
        device,
        observed_tier,
        active_app,
        apps,
        media_player,
        tv_active_channel,
        observed_at: unix_seconds(),
    })
}

async fn poll_inventory_once(
    cx: &asupersync::Cx,
    store: &Arc<Mutex<SqliteStore>>,
    transport: &Arc<SelectedTransport>,
    inventory: &Arc<Mutex<Inventory>>,
) {
    let rows = match store
        .lock()
        .map_err(|_| "inventory store lock poisoned".to_string())
        .and_then(|store| store.list_devices().map_err(|error| error.to_string()))
    {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("zestroku serve: inventory poll could not list devices: {error}");
            return;
        }
    };

    for row in rows {
        if cx.checkpoint().is_err() {
            return;
        }
        let Some(port) = row_port(&row) else {
            eprintln!("zestroku serve: invalid inventory device target");
            continue;
        };
        let device_id = device_id(&row.ip, port);
        if device_id != row.id {
            eprintln!("zestroku serve: inventory device identity mismatch");
            continue;
        }
        let now = unix_seconds();
        let generation = match inventory.lock() {
            Ok(inventory) => inventory.poll_generation_if_due(&device_id, now),
            Err(_) => {
                eprintln!("zestroku serve: inventory state lock poisoned");
                return;
            }
        };
        let Some(generation) = generation else {
            continue;
        };
        match poll_device_state(store, transport, &row.ip, port, &device_id).await {
            Ok(snapshot) => match inventory.lock() {
                Ok(mut inventory) => {
                    if !inventory.record_state_for_poll(snapshot, generation) {
                        continue;
                    }
                }
                Err(_) => {
                    eprintln!("zestroku serve: inventory state lock poisoned");
                    return;
                }
            },
            Err(error) => {
                match inventory.lock() {
                    Ok(mut inventory) => {
                        if inventory
                            .record_poll_failure_for_poll(&device_id, unix_seconds(), generation)
                            .is_none()
                        {
                            continue;
                        }
                    }
                    Err(_) => {
                        eprintln!("zestroku serve: inventory state lock poisoned");
                        return;
                    }
                }
                eprintln!("zestroku serve: inventory refresh failed: {error}");
            }
        }
    }
}

async fn poll_inventory_forever(
    cx: &asupersync::Cx,
    store: Arc<Mutex<SqliteStore>>,
    transport: Arc<SelectedTransport>,
    inventory: Arc<Mutex<Inventory>>,
) {
    loop {
        if cx.checkpoint().is_err() {
            return;
        }
        poll_inventory_once(cx, &store, &transport, &inventory).await;
        if cx.checkpoint().is_err() {
            return;
        }
        asupersync::time::sleep(asupersync::time::wall_now(), Duration::from_secs(5)).await;
    }
}
async fn serve_until_shutdown(
    app: fastapi::App,
    bind_addr: String,
    config: ServerConfig,
    store: Arc<Mutex<SqliteStore>>,
    transport: Arc<SelectedTransport>,
    inventory: Arc<Mutex<Inventory>>,
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

    let poll_and_signal = {
        let mut polling = std::pin::pin!(poll_inventory_forever(
            &cx,
            Arc::clone(&store),
            Arc::clone(&transport),
            inventory,
        ));
        let mut serving_or_polling = std::pin::pin!(async {
            match Select::new(serving.as_mut(), polling.as_mut()).await {
                Ok(Either::Left(result)) => {
                    result.map_err(|error| format!("HTTP server failed: {error}"))
                }
                Ok(Either::Right(())) => serving
                    .as_mut()
                    .await
                    .map_err(|error| format!("HTTP server failed: {error}")),
                Err(error) => Err(format!("server/poll selection failed: {error:?}")),
            }
        });
        match Select::new(serving_or_polling.as_mut(), receiving_signal.as_mut()).await {
            Ok(Either::Left(result)) => Some(result),
            Ok(Either::Right(Ok(_))) => None,
            Ok(Either::Right(Err(error))) => {
                Some(Err(format!("signal selection failed: {error:?}")))
            }
            Err(error) => Some(Err(format!("shutdown selection failed: {error:?}"))),
        }
    };
    let result = match poll_and_signal {
        Some(result) => result,
        None => {
            server.shutdown();
            serving
                .as_mut()
                .await
                .map_err(|error| format!("HTTP server drain failed: {error}"))
        }
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
    let inventory = Arc::new(Mutex::new(Inventory::new()));
    let app = zestroku_api::build_app_with_inventory(
        Arc::clone(&store),
        Arc::clone(&transport),
        Arc::clone(&inventory),
    );
    let bind_addr = addr.to_string();
    let mut config = ServerConfig::new(bind_addr.clone());
    if !addr.ip().is_unspecified() {
        config = config.with_allowed_hosts([addr.ip().to_string()]);
    }
    let result = runtime.block_on(serve_until_shutdown(
        app, bind_addr, config, store, transport, inventory, opts,
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
