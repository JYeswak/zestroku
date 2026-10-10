//! zestroku-api — read-only HTTP routes over the shared inventory and ECP transport.
#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use fastapi::core::{App, PathParams, Request, RequestContext, Response, ResponseBody, StatusCode};
use serde::Serialize;
use zestroku_core::store::{DeviceRow, Store};
use zestroku_core::{check_tier, discovery::ECP_DEFAULT_PORT};
use zestroku_proto::{
    ActiveApp, Command, DeviceInfo, EcpGate, EcpResponse, Transport, parse_active_app,
    parse_device_info, parse_ecp_response, query,
};
use zestroku_types::{AccessTier, Device, Error, Result};

#[derive(Debug)]
struct ApiState<S, T> {
    store: Arc<Mutex<S>>,
    transport: Arc<T>,
}

/// Build the read-only v1 HTTP API over a shared store and ECP transport.
///
/// It registers `GET /v1/devices`, `GET /v1/devices/{host}/info`, and
/// `GET /v1/devices/{host}/active-app`. Host-specific reads return 404 unless
/// the host is already in the store; non-open ECP responses map to 502.
/// The store mutex is held only for synchronous store calls and is released
/// before any transport await. Host-specific reads are limited to devices
/// already present in the inventory.
pub fn build_app<S, T>(store: Arc<Mutex<S>>, transport: Arc<T>) -> App
where
    S: Store + Send + 'static,
    T: Transport + 'static,
{
    let state = Arc::new(ApiState { store, transport });

    let devices_state = Arc::clone(&state);
    let info_state = Arc::clone(&state);
    let active_app_state = Arc::clone(&state);

    App::builder()
        .get(
            "/v1/devices",
            move |_ctx: &RequestContext, _req: &mut Request| {
                let state = Arc::clone(&devices_state);
                async move { list_devices(&state) }
            },
        )
        .get(
            "/v1/devices/{host}/info",
            move |_ctx: &RequestContext, req: &mut Request| {
                let host = path_host(req);
                let state = Arc::clone(&info_state);
                async move {
                    match host {
                        Some(host) => device_info(&state, &host).await,
                        None => error_response(StatusCode::BAD_REQUEST, "missing_host"),
                    }
                }
            },
        )
        .get(
            "/v1/devices/{host}/active-app",
            move |_ctx: &RequestContext, req: &mut Request| {
                let host = path_host(req);
                let state = Arc::clone(&active_app_state);
                async move {
                    match host {
                        Some(host) => active_app(&state, &host).await,
                        None => error_response(StatusCode::BAD_REQUEST, "missing_host"),
                    }
                }
            },
        )
        .build()
}

#[derive(Debug, Serialize)]
struct DeviceSummary {
    name: String,
    host: String,
    model: String,
    model_number: String,
    software_version: String,
    access_tier: String,
    last_seen: i64,
}

#[derive(Debug, Serialize)]
struct DeviceInfoView {
    host: String,
    model_name: String,
    model_number: String,
    software_version: String,
    is_tv: bool,
}

#[derive(Debug, Serialize)]
struct ActiveAppView {
    host: String,
    app_id: Option<u32>,
    name: String,
}

#[derive(Debug, Serialize)]
struct ErrorView {
    error: &'static str,
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

fn path_host(req: &Request) -> Option<String> {
    req.get_extension::<PathParams>()
        .and_then(|params| params.get("host"))
        .map(str::to_owned)
}

fn device_for_gate(row: &DeviceRow) -> Device {
    Device {
        host: row.ip.clone(),
        port: ECP_DEFAULT_PORT,
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

fn find_device<S: Store, T>(state: &ApiState<S, T>, host: &str) -> Result<Option<DeviceRow>> {
    let store = state
        .store
        .lock()
        .map_err(|_| Error::Unreachable("inventory store lock poisoned".into()))?;
    store
        .list_devices()
        .map(|rows| rows.into_iter().find(|row| row.ip == host))
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

async fn device_info<S: Store, T: Transport>(state: &ApiState<S, T>, host: &str) -> Response {
    let row = match find_device(state, host) {
        Ok(Some(row)) => row,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
    };
    let device = device_for_gate(&row);
    let command = query("device-info");
    let response = match send_checked(state, &device, &command).await {
        Ok(response) if parse_ecp_response(&response).gate == EcpGate::Open => response,
        Ok(_) => return error_response(StatusCode::from_u16(502), "ecp_response"),
        Err(Error::TierRefused { .. }) => {
            return error_response(StatusCode::FORBIDDEN, "tier_refused");
        }
        Err(_) => return error_response(StatusCode::from_u16(502), "transport_error"),
    };
    match parse_device_info(&response.body) {
        Ok(info) => json_response(StatusCode::OK, &device_info_view(host, info)),
        Err(_) => error_response(StatusCode::from_u16(502), "malformed_device_info"),
    }
}

async fn active_app<S: Store, T: Transport>(state: &ApiState<S, T>, host: &str) -> Response {
    let row = match find_device(state, host) {
        Ok(Some(row)) => row,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
    };
    let device = device_for_gate(&row);
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
        Ok(app) => json_response(StatusCode::OK, &active_app_view(host, app)),
        Err(_) => error_response(StatusCode::from_u16(502), "malformed_active_app"),
    }
}

fn device_info_view(host: &str, info: DeviceInfo) -> DeviceInfoView {
    DeviceInfoView {
        host: host.to_owned(),
        model_name: info.model_name,
        model_number: info.model_number,
        software_version: info.software_version,
        is_tv: info.is_tv,
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

fn error_response(status: StatusCode, code: &'static str) -> Response {
    json_response(status, &ErrorView { error: code })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use fastapi::TestClient;
    use zestroku_core::store::{DeviceRow, SqliteStore, Store};
    use zestroku_proto::{Command, EcpResponse, Transport, keypress};
    use zestroku_types::{AccessTier, Device, Error, Result};

    use super::{build_app, gate_before_dispatch};

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
        EcpResponse {
            status: 200,
            headers: Vec::new(),
            body: body.to_owned(),
        }
    }

    fn store_with_device() -> (tempfile::TempDir, Arc<Mutex<SqliteStore>>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteStore::open(&dir.path().join("api.db")).expect("open store");
        store
            .upsert_device(&DeviceRow {
                id: "fixture-row".into(),
                name: "Fixture Roku".into(),
                ip: "192.0.2.10".into(),
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

        let response = client.get("/v1/devices").send();
        assert_eq!(response.status().as_u16(), 200);
        let devices: serde_json::Value =
            serde_json::from_str(&response.text()).expect("device JSON");
        assert_eq!(devices[0]["host"], "192.0.2.10");
        assert_eq!(devices[0]["name"], "Fixture Roku");
        assert_eq!(devices[0]["access_tier"], "permissive");
        assert_eq!(devices[0]["last_seen"], 1_700_000_000_i64);
        assert!(devices[0].get("id").is_none());
        assert!(devices[0].get("serial").is_none());
    }

    #[test]
    fn device_info_and_active_app_routes_dispatch_read_only_queries() {
        let (_dir, store) = store_with_device();
        let transport = Arc::new(FakeTransport::new([
            response(DEVICE_INFO_XML),
            response(ACTIVE_APP_XML),
        ]));
        let client = TestClient::new(build_app(store, Arc::clone(&transport)));

        let info = client.get("/v1/devices/192.0.2.10/info").send();
        assert_eq!(info.status().as_u16(), 200);
        let info_json: serde_json::Value = serde_json::from_str(&info.text()).expect("info JSON");
        assert_eq!(info_json["host"], "192.0.2.10");
        assert_eq!(info_json["model_name"], "Roku Express 4K+");
        assert_eq!(info_json["model_number"], "9999X");
        assert_eq!(info_json["software_version"], "0.0.0");
        assert_eq!(info_json["is_tv"], false);

        let active_app = client.get("/v1/devices/192.0.2.10/active-app").send();
        assert_eq!(active_app.status().as_u16(), 200);
        let active_json: serde_json::Value =
            serde_json::from_str(&active_app.text()).expect("active-app JSON");
        assert_eq!(active_json["host"], "192.0.2.10");
        assert_eq!(active_json["app_id"], 12);
        assert_eq!(active_json["name"], "Fixture App");

        let calls = transport.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "192.0.2.10");
        assert_eq!(calls[0].2.path, "/query/device-info");
        assert_eq!(calls[0].2.method, zestroku_proto::Method::Get);
        assert_eq!(calls[1].2.path, "/query/active-app");
        assert_eq!(calls[1].2.method, zestroku_proto::Method::Get);
    }

    #[test]
    fn unknown_host_returns_not_found_without_dispatch() {
        let (_dir, store) = store_with_device();
        let transport = Arc::new(FakeTransport::new([]));
        let client = TestClient::new(build_app(store, Arc::clone(&transport)));

        let response = client.get("/v1/devices/192.0.2.99/info").send();
        assert_eq!(response.status().as_u16(), 404);
        assert!(transport.calls().is_empty());
    }

    #[test]
    fn tier_refusal_happens_before_transport_dispatch() {
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
        let mut calls = 0;
        let result = gate_before_dispatch(&device, &command, || {
            calls += 1;
            ()
        });

        assert!(matches!(result, Err(Error::TierRefused { .. })));
        assert_eq!(calls, 0);
    }
}
