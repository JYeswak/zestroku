//! zestroku-api — inventory reads and per-operation-approved HTTP mutations.
#![forbid(unsafe_code)]

mod routes;

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use fastapi::core::{App, PathParams, Request, Response, ResponseBody, StatusCode};
use fastapi::fastapi_openapi;
use serde::Serialize;
use zestroku_core::store::{DeviceRow, Store};
use zestroku_core::{
    DeviceStateSnapshot, Inventory, InventoryStateView, check_tier, discovery::ECP_DEFAULT_PORT,
};
use zestroku_proto::{
    ActiveApp, Command, DeviceInfo, EcpGate, EcpResponse, Transport, parse_active_app,
    parse_device_info, parse_ecp_response, query,
};
use zestroku_types::{AccessTier, Device, Error, ErrorCode, Result};
#[derive(Debug)]
struct ApiState<S, T> {
    store: Arc<Mutex<S>>,
    transport: Arc<T>,
    inventory: Arc<Mutex<Inventory>>,
}

/// Build the v1 HTTP API over a shared store and ECP transport without a
/// background inventory poller. Fresh reads still use any state supplied by a
/// caller through [`build_app_with_inventory`]; otherwise existing live reads
/// remain available for embedding and API tests.
pub fn build_app<S, T>(store: Arc<Mutex<S>>, transport: Arc<T>) -> App
where
    S: Store + Send + 'static,
    T: Transport + 'static,
{
    build_app_with_inventory(store, transport, Arc::new(Mutex::new(Inventory::new())))
}

/// Build the v1 HTTP API against the daemon's shared per-device inventory.
///
/// Registers health, inventory, device read/state, OpenAPI, and
/// confirmation-gated mutation routes. Mutation targets come only from the
/// inventory path parameter; request bodies cannot override host, port, tier,
/// or client provenance. Mutations snapshot active-app state, then delegate
/// tier-before-token authorization and durable result recording to the core
/// gate. No route mints approvals. Bodies are strict JSON objects; a present
/// Origin must match `http://<Host>` before parsing, token consumption, or ECP
/// I/O. Missing Origin is allowed for non-browser clients, and no
/// Access-Control-Allow-Origin header is emitted. Store locks are released
/// before transport awaits. The daemon shares this inventory with its poll
/// loop so fresh reads need no additional ECP round-trip.
pub fn build_app_with_inventory<S, T>(
    store: Arc<Mutex<S>>,
    transport: Arc<T>,
    inventory: Arc<Mutex<Inventory>>,
) -> App
where
    S: Store + Send + 'static,
    T: Transport + 'static,
{
    routes::build_app(store, transport, inventory)
}

pub(super) fn cached_inventory_state<S, T>(
    state: &ApiState<S, T>,
    device_id: &str,
) -> std::result::Result<Option<InventoryStateView>, ()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    state
        .inventory
        .lock()
        .map(|inventory| inventory.state_at(device_id, now))
        .map_err(|_| ())
}

#[derive(Debug, Serialize, fastapi::JsonSchema)]
struct HealthView {
    status: &'static str,
}
/// Bind guard for the daemon listener: loopback and Tailscale ranges only.
/// A wildcard (`0.0.0.0`, `::`) listens on every interface and is never
/// accepted; RFC 1918, link-local, public and IPv4-mapped IPv6 addresses are
/// refused. Tailnet membership is NOT decided here: a tailnet-range bind
/// additionally requires own-node membership (see `node_owns_address`),
/// checked at startup. Returns the parsed socket address.
pub fn check_bind(bind: &str) -> Result<std::net::SocketAddr> {
    let addr: std::net::SocketAddr = bind
        .parse()
        .map_err(|_| Error::Malformed(format!("--bind {bind:?}: expected ip:port")))?;
    if !bind_ip_allowed(&addr.ip()) {
        return Err(Error::Malformed(format!(
            "--bind {bind}: only loopback and this node's Tailscale addresses are served"
        )));
    }
    Ok(addr)
}

/// Pure range policy: IPv4 loopback, IPv6 loopback, the 100.64.0.0/10
/// shared-address range and fd7a:115c:a1e0::/48. Everything else,
/// including IPv4-mapped IPv6 (even mapped loopback), is refused.
fn bind_ip_allowed(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            if v4.is_loopback() {
                return true;
            }
            let o = v4.octets();
            o[0] == 100 && (64..=127).contains(&o[1])
        }
        std::net::IpAddr::V6(v6) => {
            if v6.to_ipv4_mapped().is_some() {
                return false;
            }
            if v6.is_loopback() {
                return true;
            }
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// Parse the `TailscaleIPs` array from `tailscale status --json` output.
pub fn parse_tailscale_ips(json: &str) -> Result<Vec<std::net::IpAddr>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("tailscale status is not JSON: {e}"))?;
    let ips = value
        .get("TailscaleIPs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "tailscale status has no TailscaleIPs array".to_string())?;
    ips.iter()
        .map(|entry| {
            entry
                .as_str()
                .ok_or_else(|| "tailscale address is not a string".to_string())
                .and_then(|text| {
                    text.parse()
                        .map_err(|_| format!("tailscale address is not an IP: {text:?}"))
                })
        })
        .collect()
}

/// Own-node membership: the bind address must equal one of this node's
/// Tailscale addresses. Range membership alone is not enough because
/// RFC 6598 CGNAT space is not Tailscale-exclusive.
pub fn node_owns_address(ip: &std::net::IpAddr, self_ips: &[std::net::IpAddr]) -> bool {
    self_ips.contains(ip)
}

/// Read this node's Tailscale addresses from the local Tailscale source.
///
/// A five-second deadline covers stdout EOF and child completion together.
/// Cancellation drops the owned pipe before killing and reaping the child;
/// no blocking reader thread can outlive the lookup.
pub async fn tailscale_self_ips() -> Result<Vec<std::net::IpAddr>, String> {
    use asupersync::io::AsyncReadExt;
    use asupersync::process::{Command, Stdio};
    use asupersync::time::{sleep, timeout, wall_now};
    use std::time::Duration;

    let mut child = Command::new("tailscale")
        .args(["status", "--json"])
        .stdin(Stdio::Null)
        .stdout(Stdio::Pipe)
        .stderr(Stdio::Null)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("tailscale status failed to run: {error}"))?;
    let output = {
        let mut stdout = child.stdout().expect("Tailscale stdout was piped");
        let operation = async {
            let mut bytes = Vec::new();
            stdout
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| format!("tailscale status read failed: {error}"))?;
            let status = loop {
                if let Some(status) = child
                    .try_wait()
                    .map_err(|error| format!("tailscale status wait failed: {error}"))?
                {
                    break status;
                }
                sleep(wall_now(), Duration::from_millis(20)).await;
            };
            Ok::<_, String>((status, bytes))
        };
        timeout(wall_now(), Duration::from_secs(5), operation)
            .await
            .map_err(|_| "tailscale status timed out after 5 seconds".to_string())
            .and_then(|result| result)
    };
    let (status, bytes) = match output {
        Ok(output) => output,
        Err(error) => {
            if child.id().is_some() {
                let kill = child.kill();
                if let Err(wait) = child.wait() {
                    return Err(format!("{error}; child cleanup kill={kill:?}, wait={wait}"));
                }
            }
            return Err(error);
        }
    };
    if !status.success() {
        return Err("tailscale status exited unsuccessfully".to_string());
    }
    let text = String::from_utf8(bytes).map_err(|_| "tailscale status is not UTF-8".to_string())?;
    parse_tailscale_ips(&text)
}

#[derive(Debug, Serialize, fastapi::JsonSchema)]
struct DeviceSummary {
    name: String,
    host: String,
    model: String,
    model_number: String,
    software_version: String,
    access_tier: String,
    last_seen: i64,
}

#[derive(Debug, Serialize, fastapi::JsonSchema)]
struct DeviceInfoView {
    host: String,
    model_name: String,
    model_number: String,
    software_version: String,
    is_tv: bool,
    access_tier: String,
    tv_active_channel: Option<bool>,
}

#[derive(Debug, Serialize, fastapi::JsonSchema)]
struct ActiveAppView {
    host: String,
    app_id: Option<u32>,
    name: String,
}

#[derive(Debug, Serialize, fastapi::JsonSchema)]
struct ErrorView {
    /// Existing operation-specific cause; retained for current consumers.
    error: &'static str,
    /// Canonical shared code, absent when the cause has no honest mapping.
    code: Option<&'static str>,
    detail: String,
    hint: Option<&'static str>,
    suggestions: Vec<String>,
}

fn list_devices<S: Store, T>(state: &ApiState<S, T>) -> Response {
    let rows = match state.store.lock() {
        Ok(store) => match store.list_devices() {
            Ok(rows) => rows,
            Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
        },
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
    };

    let devices: Vec<_> = rows
        .into_iter()
        .map(|row| DeviceSummary {
            name: row.name,
            host: row.ip,
            model: row.model,
            model_number: row.model_num,
            software_version: row.firmware,
            access_tier: row.access_tier,
            last_seen: row.last_seen,
        })
        .collect();
    json_response(StatusCode::OK, &devices)
}

fn path_target(req: &Request) -> Option<String> {
    req.get_extension::<PathParams>()
        .and_then(|params| params.get("host"))
        .map(str::to_owned)
}

#[derive(Debug)]
struct DeviceTarget {
    port: u16,
    id: String,
}

fn parse_device_target(raw: &str) -> Option<DeviceTarget> {
    if raw.is_empty()
        || raw
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\' | b'?' | b'#'))
    {
        return None;
    }

    let (host, port) = if let Some(bracketed) = raw.strip_prefix('[') {
        let close = bracketed.find(']')?;
        let host = &bracketed[..close];
        let suffix = &bracketed[close + 1..];
        let port = if suffix.is_empty() {
            ECP_DEFAULT_PORT
        } else {
            suffix.strip_prefix(':')?.parse::<u16>().ok()?
        };
        (host.to_owned(), port)
    } else if let Some((host, port)) = raw.rsplit_once(':') {
        if !host.contains(':') {
            (host.to_owned(), port.parse::<u16>().ok()?)
        } else if raw.parse::<std::net::Ipv6Addr>().is_ok() {
            (raw.to_owned(), ECP_DEFAULT_PORT)
        } else {
            return None;
        }
    } else {
        (raw.to_owned(), ECP_DEFAULT_PORT)
    };
    if host.is_empty() || port == 0 {
        return None;
    }
    let id = zestroku_core::tier::device_id(&host, port);
    Some(DeviceTarget { port, id })
}

fn device_for_gate(row: &DeviceRow, target: &DeviceTarget) -> Device {
    Device {
        host: row.ip.clone(),
        port: target.port,
        model_name: row.model.clone(),
        model_number: row.model_num.clone(),
        software_version: row.firmware.clone(),
        // The stored row has no TV-class field; check_tier only reads the tier.
        is_tv: false,
        access_tier: if row.access_tier.eq_ignore_ascii_case("permissive") {
            AccessTier::Permissive
        } else {
            AccessTier::Open
        },
    }
}

fn find_device<S: Store, T>(
    state: &ApiState<S, T>,
    target: &DeviceTarget,
) -> Result<Option<DeviceRow>> {
    let store = state
        .store
        .lock()
        .map_err(|_| Error::Unreachable("inventory store lock poisoned".into()))?;
    store
        .get_device(&target.id)
        .map_err(|error| Error::Unreachable(format!("inventory store: {error}")))
}

fn gate_before_dispatch<F>(
    device: &Device,
    command: &Command,
    dispatch: impl FnOnce() -> F,
) -> Result<F> {
    check_tier(command, device)?;
    Ok(dispatch())
}

async fn send_checked<T: Transport>(
    state: &ApiState<impl Store, T>,
    device: &Device,
    command: &Command,
) -> Result<EcpResponse> {
    gate_before_dispatch(device, command, || {
        state.transport.send(&device.host, device.port, command)
    })?
    .await
}

async fn device_info<S: Store, T: Transport>(state: &ApiState<S, T>, raw_target: &str) -> Response {
    let target = match parse_device_target(raw_target) {
        Some(target) => target,
        None => return error_response(StatusCode::BAD_REQUEST, "invalid_device_target"),
    };
    let row = match find_device(state, &target) {
        Ok(Some(row)) => row,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
    };
    let device = device_for_gate(&row, &target);
    let cached = match cached_inventory_state(state, &target.id) {
        Ok(cached) => cached,
        Err(()) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
    };
    if let Some(cached) = cached.filter(|state| state.is_fresh) {
        return json_response(
            StatusCode::OK,
            &device_info_view_from_snapshot(&cached.snapshot),
        );
    }
    let command = query("device-info");
    let response = match send_checked(state, &device, &command).await {
        Ok(response) if parse_ecp_response(&response).gate == EcpGate::Open => response,
        Ok(_) => return error_response(StatusCode::from_u16(502), "ecp_response"),
        Err(Error::TierRefused { .. }) => {
            return error_response(StatusCode::FORBIDDEN, "tier_refused");
        }
        Err(_) => return error_response(StatusCode::from_u16(502), "transport_error"),
    };
    let info = match parse_device_info(&response.body) {
        Ok(info) => info,
        Err(_) => return error_response(StatusCode::from_u16(502), "malformed_device_info"),
    };

    // Match the CLI info probe: tier comes from the live apps response, not
    // a potentially stale inventory value. This is a read-only ECP probe.
    let apps = state
        .transport
        .send(&device.host, device.port, &query("apps"))
        .await;
    let access_tier = match &apps {
        Ok(response) => {
            let observed = zestroku_core::tier::classify_tier_from_response(response);
            let tier = observed.as_str().to_owned();
            let outcome = format!("{}", parse_ecp_response(response).gate as i32);
            if let Ok(store) = state.store.lock() {
                let _ = zestroku_core::tier::record_probe(&*store, &target.id, observed, &outcome);
            }
            tier
        }
        Err(_) => "unknown".to_owned(),
    };
    let tv_channel = match state
        .transport
        .send(&device.host, device.port, &query("tv-active-channel"))
        .await
    {
        Ok(response) => match parse_ecp_response(&response).gate {
            EcpGate::Open | EcpGate::Limited403 => Some(true),
            EcpGate::Absent404 => Some(false),
            _ => None,
        },
        Err(_) => None,
    };
    json_response(
        StatusCode::OK,
        &device_info_view(&device.host, info, access_tier, tv_channel),
    )
}

async fn active_app<S: Store, T: Transport>(state: &ApiState<S, T>, raw_target: &str) -> Response {
    let target = match parse_device_target(raw_target) {
        Some(target) => target,
        None => return error_response(StatusCode::BAD_REQUEST, "invalid_device_target"),
    };
    let row = match find_device(state, &target) {
        Ok(Some(row)) => row,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
    };
    let device = device_for_gate(&row, &target);
    let cached = match cached_inventory_state(state, &target.id) {
        Ok(cached) => cached,
        Err(()) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
    };
    if let Some(cached) = cached.filter(|state| state.is_fresh) {
        return json_response(
            StatusCode::OK,
            &active_app_view(
                &cached.snapshot.device.host,
                cached.snapshot.active_app.clone(),
            ),
        );
    }
    let command = query("active-app");
    let response = match send_checked(state, &device, &command).await {
        Ok(response) if parse_ecp_response(&response).gate == EcpGate::Open => response,
        Ok(_) => return error_response(StatusCode::from_u16(502), "ecp_response"),
        Err(Error::TierRefused { .. }) => {
            return error_response(StatusCode::FORBIDDEN, "tier_refused");
        }
        Err(_) => return error_response(StatusCode::from_u16(502), "transport_error"),
    };
    match parse_active_app(&response.body) {
        Ok(app) => json_response(StatusCode::OK, &active_app_view(&device.host, app)),
        Err(_) => error_response(StatusCode::from_u16(502), "malformed_active_app"),
    }
}

fn device_info_view(
    host: &str,
    info: DeviceInfo,
    access_tier: String,
    tv_active_channel: Option<bool>,
) -> DeviceInfoView {
    DeviceInfoView {
        host: host.to_owned(),
        model_name: info.model_name,
        model_number: info.model_number,
        software_version: info.software_version,
        is_tv: info.is_tv,
        access_tier,
        tv_active_channel,
    }
}
fn device_info_view_from_snapshot(snapshot: &DeviceStateSnapshot) -> DeviceInfoView {
    DeviceInfoView {
        host: snapshot.device.host.clone(),
        model_name: snapshot.device.model_name.clone(),
        model_number: snapshot.device.model_number.clone(),
        software_version: snapshot.device.software_version.clone(),
        is_tv: snapshot.device.is_tv,
        access_tier: snapshot.observed_tier.as_str().to_owned(),
        tv_active_channel: snapshot.tv_active_channel,
    }
}

fn active_app_view(host: &str, app: ActiveApp) -> ActiveAppView {
    ActiveAppView {
        host: host.to_owned(),
        app_id: app.app_id,
        name: app.name,
    }
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(body) => Response::with_status(status)
            .header("content-type", b"application/json".to_vec())
            .body(ResponseBody::Bytes(body)),
        Err(_) => Response::with_status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

fn semantic_error_code(cause: &str) -> Option<ErrorCode> {
    match cause {
        "device_not_found" | "unknown_device" => Some(ErrorCode::UnknownDevice),
        "ambiguous_name" => Some(ErrorCode::AmbiguousName),
        "DEVICE_UNREACHABLE" | "transport_error" => Some(ErrorCode::DeviceUnreachable),
        "tier_refused" => Some(ErrorCode::TierRefused),
        "confirmation_required" => Some(ErrorCode::ConfirmationRequired),
        "policy_denied" => Some(ErrorCode::PolicyDenied),
        "app_not_installed" | "undo_app_not_installed" => Some(ErrorCode::AppNotInstalled),
        "store_error" | "action_log_error" => Some(ErrorCode::StoreUnavailable),
        "daemon_unavailable" => Some(ErrorCode::DaemonUnavailable),
        _ => None,
    }
}

fn error_detail(cause: &str) -> String {
    match cause {
        "device_not_found" => "No matching device is present in the daemon inventory.".to_owned(),
        "undo_app_not_installed" => {
            "The previously active app is not installed, so it cannot be restored.".to_owned()
        }
        "tier_refused" => {
            "The device refused this operation under its observed Limited tier.".to_owned()
        }
        "DEVICE_UNREACHABLE" => {
            "No completed device-state snapshot is available for this target.".to_owned()
        }
        "store_error" | "action_log_error" => {
            "The daemon could not read or update its local store.".to_owned()
        }
        other => other.replace('_', " "),
    }
}

fn error_response(status: StatusCode, cause: &'static str) -> Response {
    let semantic = semantic_error_code(cause);
    json_response(
        status,
        &ErrorView {
            error: cause,
            code: semantic.map(ErrorCode::as_str),
            detail: error_detail(cause),
            hint: semantic.map(ErrorCode::hint),
            suggestions: Vec::new(),
        },
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use fastapi::TestClient;
    use zestroku_core::store::{DeviceRow, SqliteStore, Store};
    use zestroku_core::tier::ObservedTier;
    use zestroku_core::{AppsSnapshot, DeviceStateSnapshot, Inventory, MediaPlayerSnapshot};
    use zestroku_proto::{
        ActiveApp, Command, EcpResponse, InstalledApp, MediaPlayerState, Transport, keypress,
    };
    use zestroku_types::{AccessTier, Device, Error, Result};

    use super::{ApiState, build_app, build_app_with_inventory, send_checked};

    const DEVICE_INFO_XML: &str = "<device-info><model-name>Roku Express 4K+</model-name>\
         <model-number>9999X</model-number><software-version>0.0.0</software-version>\
         <serial-number>SCRUBBED</serial-number><is-tv>false</is-tv></device-info>";
    const ACTIVE_APP_XML: &str = "<active-app><app id=\"12\">Fixture App</app></active-app>";

    struct FakeTransport {
        responses: Mutex<VecDeque<EcpResponse>>,
        calls: Mutex<Vec<(String, u16, Command)>>,
    }

    impl FakeTransport {
        fn new(responses: impl IntoIterator<Item = EcpResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<(String, u16, Command)> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl Transport for FakeTransport {
        async fn send(&self, host: &str, port: u16, command: &Command) -> Result<EcpResponse> {
            self.calls
                .lock()
                .expect("calls lock")
                .push((host.to_owned(), port, command.clone()));
            self.responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .ok_or_else(|| Error::Unreachable("fixture response exhausted".into()))
        }
    }

    fn response(body: &str) -> EcpResponse {
        response_with_status(200, body)
    }

    fn response_with_status(status: u16, body: &str) -> EcpResponse {
        EcpResponse {
            status,
            headers: Vec::new(),
            body: body.to_owned(),
        }
    }

    fn store_with_device() -> (tempfile::TempDir, Arc<Mutex<SqliteStore>>) {
        store_with_device_at("192.0.2.10", zestroku_core::discovery::ECP_DEFAULT_PORT)
    }

    fn store_with_device_at(host: &str, port: u16) -> (tempfile::TempDir, Arc<Mutex<SqliteStore>>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteStore::open(&dir.path().join("api.db")).expect("open store");
        store
            .upsert_device(&DeviceRow {
                id: zestroku_core::tier::device_id(host, port),
                name: "Fixture Roku".into(),
                ip: host.into(),
                model: "Roku Fixture".into(),
                model_num: "9999X".into(),
                firmware: "0.0.0".into(),
                wifi_driver: "fixture".into(),
                access_tier: "permissive".into(),
                last_seen: 1_700_000_000,
            })
            .expect("seed device");
        (dir, Arc::new(Mutex::new(store)))
    }

    #[test]
    fn devices_route_lists_store_rows_without_device_identifiers() {
        let (_dir, store) = store_with_device();
        let transport = Arc::new(FakeTransport::new([]));
        let client = TestClient::new(build_app(store, transport));

        let response = client
            .get("/v1/devices")
            .header_str("host", "192.0.2.10")
            .send();
        assert_eq!(response.status().as_u16(), 200);
        let devices: serde_json::Value =
            serde_json::from_str(response.text()).expect("device JSON");
        assert_eq!(devices[0]["host"], "192.0.2.10");
        assert_eq!(devices[0]["name"], "Fixture Roku");
        assert_eq!(devices[0]["access_tier"], "permissive");
        assert_eq!(devices[0]["last_seen"], 1_700_000_000_i64);
        assert!(devices[0].get("id").is_none());
        assert!(devices[0].get("serial").is_none());
    }

    #[test]
    fn device_info_and_active_app_routes_dispatch_read_only_queries() {
        let port = 18060;
        let (_dir, store) = store_with_device_at("192.0.2.10", port);
        let transport = Arc::new(FakeTransport::new([
            response(DEVICE_INFO_XML),
            response("<apps/>"),
            response_with_status(404, ""),
            response(ACTIVE_APP_XML),
        ]));
        let client = TestClient::new(build_app(store, Arc::clone(&transport)));

        let info = client
            .get(&format!("/v1/devices/192.0.2.10:{port}/info"))
            .header_str("host", "192.0.2.10")
            .send();
        assert_eq!(info.status().as_u16(), 200);
        let info_json: serde_json::Value = serde_json::from_str(info.text()).expect("info JSON");
        assert_eq!(info_json["host"], "192.0.2.10");
        assert_eq!(info_json["model_name"], "Roku Express 4K+");
        assert_eq!(info_json["model_number"], "9999X");
        assert_eq!(info_json["software_version"], "0.0.0");
        assert_eq!(info_json["is_tv"], false);
        assert_eq!(info_json["access_tier"], "permissive");
        assert_eq!(info_json["tv_active_channel"], false);

        let active_app = client
            .get(&format!("/v1/devices/192.0.2.10:{port}/active-app"))
            .header_str("host", "192.0.2.10")
            .send();
        assert_eq!(active_app.status().as_u16(), 200);
        let active_json: serde_json::Value =
            serde_json::from_str(active_app.text()).expect("active-app JSON");
        assert_eq!(active_json["host"], "192.0.2.10");
        assert_eq!(active_json["app_id"], 12);
        assert_eq!(active_json["name"], "Fixture App");

        let calls = transport.calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].0, "192.0.2.10");
        assert_eq!(calls[0].1, port);
        assert_eq!(calls[0].2.path, "/query/device-info");
        assert_eq!(calls[0].2.method, zestroku_proto::Method::Get);
        assert_eq!(calls[1].2.path, "/query/apps");
        assert_eq!(calls[2].2.path, "/query/tv-active-channel");
        assert_eq!(calls[3].2.path, "/query/active-app");
        assert!(calls.iter().all(|call| call.1 == port));
    }

    #[test]
    fn fresh_inventory_serves_read_routes_without_ecp_roundtrips() {
        let port = 18070;
        let (_dir, store) = store_with_device_at("192.0.2.10", port);
        let transport = Arc::new(FakeTransport::new([]));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("wall clock")
            .as_secs() as i64;
        let mut inventory = Inventory::new();
        inventory.record_state(DeviceStateSnapshot {
            device: Device {
                host: "192.0.2.10".into(),
                port,
                model_name: "Roku Express 4K+".into(),
                model_number: "9999X".into(),
                software_version: "15.2.4".into(),
                is_tv: false,
                access_tier: AccessTier::Permissive,
            },
            observed_tier: ObservedTier::Permissive,
            active_app: ActiveApp {
                app_id: Some(1001),
                name: "Streaming One".into(),
            },
            apps: AppsSnapshot::Available(vec![InstalledApp {
                id: "1001".into(),
                name: "Streaming One".into(),
                app_type: "appl".into(),
                subtype: Some("svod".into()),
                version: Some("1.0".into()),
            }]),
            media_player: MediaPlayerSnapshot::Available(MediaPlayerState {
                state: "play".into(),
            }),
            tv_active_channel: Some(false),
            observed_at: now,
        });
        let client = TestClient::new(build_app_with_inventory(
            Arc::clone(&store),
            Arc::clone(&transport),
            Arc::new(Mutex::new(inventory)),
        ));

        let info = client
            .get(&format!("/v1/devices/192.0.2.10:{port}/info"))
            .header_str("host", "192.0.2.10")
            .send();
        assert_eq!(info.status().as_u16(), 200);
        let info_json: serde_json::Value = serde_json::from_str(info.text()).expect("info JSON");
        assert_eq!(info_json["model_number"], "9999X");
        assert_eq!(info_json["access_tier"], "permissive");
        assert!(info_json.get("serial_number").is_none());
        assert!(info_json.get("udn").is_none());

        let active_app = client
            .get(&format!("/v1/devices/192.0.2.10:{port}/active-app"))
            .header_str("host", "192.0.2.10")
            .send();
        assert_eq!(active_app.status().as_u16(), 200);
        let active_json: serde_json::Value =
            serde_json::from_str(active_app.text()).expect("active-app JSON");
        assert_eq!(active_json["app_id"], 1001);

        let apps = client
            .get(&format!("/v1/devices/192.0.2.10:{port}/apps"))
            .header_str("host", "192.0.2.10")
            .send();
        assert_eq!(apps.status().as_u16(), 200);
        let apps_json: serde_json::Value = serde_json::from_str(apps.text()).expect("apps JSON");
        assert_eq!(apps_json["apps"][0]["id"], "1001");

        let state = client
            .get(&format!("/v1/devices/192.0.2.10:{port}/state"))
            .header_str("host", "192.0.2.10")
            .send();
        assert_eq!(state.status().as_u16(), 200);
        let state_text = state.text().to_owned();
        let state_json: serde_json::Value =
            serde_json::from_str(&state_text).expect("device state JSON");
        assert_eq!(state_json["fresh"], true);
        assert_eq!(state_json["tv_active_channel"], false);
        assert_eq!(state_json["apps_status"], "available");
        assert_eq!(state_json["media_player_status"], "available");
        assert_eq!(state_json["media_player_state"], "play");
        assert!(!state_text.contains("SCRUBBED"));
        assert!(
            transport.calls().is_empty(),
            "fresh reads must use the inventory"
        );
    }

    #[test]
    fn info_route_reports_and_records_limited_tier_from_live_probe() {
        let port = 18061;
        let (_dir, store) = store_with_device_at("192.0.2.10", port);
        let transport = Arc::new(FakeTransport::new([
            response(DEVICE_INFO_XML),
            response_with_status(403, "ECP command not allowed in Limited mode."),
            response_with_status(403, "ECP command not allowed in Limited mode."),
        ]));
        let client = TestClient::new(build_app(Arc::clone(&store), Arc::clone(&transport)));

        let info = client
            .get(&format!("/v1/devices/192.0.2.10:{port}/info"))
            .header_str("host", "192.0.2.10")
            .send();
        assert_eq!(info.status().as_u16(), 200);
        let info_json: serde_json::Value = serde_json::from_str(info.text()).expect("info JSON");
        assert_eq!(info_json["access_tier"], "limited");
        assert_eq!(info_json["tv_active_channel"], true);

        let id = zestroku_core::tier::device_id("192.0.2.10", port);
        let tier = store
            .lock()
            .expect("store lock")
            .get_access_tier(&id)
            .expect("read tier")
            .expect("tier row");
        assert_eq!(tier.tier, "limited");
        let calls = transport.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1].2.path, "/query/apps");
        assert_eq!(calls[2].2.path, "/query/tv-active-channel");
    }
    #[test]
    fn info_route_reports_unknown_tv_channel_when_probe_fails() {
        let port = 18062;
        let (_dir, store) = store_with_device_at("192.0.2.10", port);
        let transport = Arc::new(FakeTransport::new([
            response(DEVICE_INFO_XML),
            response("<apps/>"),
        ]));
        let client = TestClient::new(build_app(store, transport));

        let info = client
            .get(&format!("/v1/devices/192.0.2.10:{port}/info"))
            .header_str("host", "192.0.2.10")
            .send();
        assert_eq!(info.status().as_u16(), 200);
        let info_json: serde_json::Value = serde_json::from_str(info.text()).expect("info JSON");
        assert!(info_json["tv_active_channel"].is_null());
    }

    #[test]
    fn unknown_hosts_return_not_found_without_dispatch_on_both_read_routes() {
        let (_dir, store) = store_with_device();
        let transport = Arc::new(FakeTransport::new([]));
        let client = TestClient::new(build_app(store, Arc::clone(&transport)));

        for path in [
            "/v1/devices/192.0.2.99/info",
            "/v1/devices/192.0.2.99/active-app",
        ] {
            let response = client.get(path).header_str("host", "192.0.2.10").send();
            assert_eq!(response.status().as_u16(), 404, "path={path}");
            assert!(transport.calls().is_empty(), "path={path}");
        }
    }

    #[test]
    fn send_checked_refuses_permissive_command_before_transport_dispatch() {
        let (_dir, store) = store_with_device();
        let transport = Arc::new(FakeTransport::new([]));
        let state = ApiState {
            store,
            transport: Arc::clone(&transport),
            inventory: Arc::new(Mutex::new(Inventory::new())),
        };
        let device = Device {
            host: "192.0.2.10".into(),
            port: 8060,
            model_name: "fixture".into(),
            model_number: "fixture".into(),
            software_version: "0.0.0".into(),
            is_tv: false,
            access_tier: AccessTier::Open,
        };
        let command = keypress(zestroku_types::Key::Home);
        let runtime = asupersync::runtime::reactor::create_reactor()
            .map_err(|e| e.to_string())
            .and_then(|reactor| {
                asupersync::runtime::RuntimeBuilder::current_thread()
                    .with_reactor(reactor)
                    .build()
                    .map_err(|e| e.to_string())
            })
            .expect("runtime");
        let result = runtime.block_on(async { send_checked(&state, &device, &command).await });

        assert!(matches!(result, Err(Error::TierRefused { .. })));
        assert!(transport.calls().is_empty());
    }

    #[test]
    fn health_route_answers_ok_without_touching_store_or_transport() {
        let (_dir, store) = store_with_device();
        let transport = Arc::new(FakeTransport::new([]));
        let client = TestClient::new(build_app(store, Arc::clone(&transport)));

        let response = client
            .get("/health")
            .header_str("host", "192.0.2.10")
            .send();
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = serde_json::from_str(response.text()).expect("health JSON");
        assert_eq!(body["status"], "ok");
        assert!(transport.calls().is_empty());
    }

    #[test]
    fn check_bind_allows_loopback_and_tailnet_ranges() {
        assert!(super::check_bind("127.0.0.1:8099").is_ok());
        assert!(super::check_bind("127.0.0.2:8099").is_ok());
        assert!(super::check_bind("[::1]:8099").is_ok());
        assert!(super::check_bind("100.64.0.0:8099").is_ok());
        assert!(super::check_bind("100.127.255.255:8099").is_ok());
        assert!(super::check_bind("100.93.84.11:8099").is_ok());
        assert!(super::check_bind("[fd7a:115c:a1e0::1]:8099").is_ok());
        assert!(super::check_bind("[fd7a:115c:a1e0:ffff:ffff:ffff:ffff:ffff]:8099").is_ok());
        assert!(
            super::check_bind("127.0.0.1:0").is_ok(),
            "port 0 stays valid"
        );
        // Planted negatives: each guard removed turns one of these red.
        assert!(super::check_bind("100.63.255.255:8099").is_err());
        assert!(super::check_bind("100.128.0.0:8099").is_err());
        assert!(
            super::check_bind("[fd7a:115c:a1df:ffff::1]:8099").is_err(),
            "adjacent /48"
        );
        assert!(
            super::check_bind("[fd7a:115c:a1e1::1]:8099").is_err(),
            "adjacent /48"
        );
        assert!(super::check_bind("192.168.1.20:8099").is_err(), "RFC1918");
        assert!(super::check_bind("10.0.0.5:8099").is_err(), "RFC1918");
        assert!(super::check_bind("169.254.1.2:8099").is_err(), "link-local");
        assert!(super::check_bind("0.0.0.0:8099").is_err(), "wildcard");
        assert!(super::check_bind("[::]:8099").is_err(), "v6 wildcard");
        assert!(super::check_bind("8.8.8.8:8099").is_err(), "public");
        assert!(
            super::check_bind("[::ffff:127.0.0.1]:8099").is_err(),
            "mapped loopback"
        );
        assert!(
            super::check_bind("[::ffff:100.64.0.1]:8099").is_err(),
            "mapped tailnet"
        );
        assert!(super::check_bind("not-an-addr").is_err(), "malformed");
    }

    #[test]
    fn tailscale_self_ips_parse_and_membership() {
        let json = r#"{"TailscaleIPs": ["100.93.84.11", "fd7a:115c:a1e0::8d3b:540c"], "BackendState": "Running"}"#;
        let ips = super::parse_tailscale_ips(json).expect("fixture parses");
        assert_eq!(ips.len(), 2);
        let own_v4: std::net::IpAddr = "100.93.84.11".parse().unwrap();
        let own_v6: std::net::IpAddr = "fd7a:115c:a1e0::8d3b:540c".parse().unwrap();
        let other_cgnat: std::net::IpAddr = "100.93.84.12".parse().unwrap();
        assert!(super::node_owns_address(&own_v4, &ips));
        assert!(super::node_owns_address(&own_v6, &ips));
        assert!(
            !super::node_owns_address(&other_cgnat, &ips),
            "non-node CGNAT is not owned"
        );
        assert!(super::parse_tailscale_ips("{}").is_err(), "missing array");
        assert!(
            super::parse_tailscale_ips(r#"{"TailscaleIPs": ["999.1.1.1"]}"#).is_err(),
            "malformed entry"
        );
        assert!(
            super::parse_tailscale_ips(r#"{"TailscaleIPs": [42]}"#).is_err(),
            "non-string entry"
        );
        assert!(super::parse_tailscale_ips("not json").is_err());
    }
}
