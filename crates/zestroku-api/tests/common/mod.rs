#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use zestroku_core::store::{AccessTierRow, DeviceRow, SqliteStore, Store};
use zestroku_proto::{Command, EcpResponse, Transport};
use zestroku_types::{Error, Result};

pub const HOST: &str = "192.0.2.10";
pub const PORT: u16 = 18_063;
pub const LIMITED_BODY: &str = "ECP command not allowed in Limited mode.";

pub struct ResponseTransport {
    responses: Mutex<VecDeque<EcpResponse>>,
    calls: Mutex<Vec<(String, u16, Command)>>,
}

impl ResponseTransport {
    pub fn new(responses: impl IntoIterator<Item = EcpResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<(String, u16, Command)> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl Transport for ResponseTransport {
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

pub fn response(status: u16, body: &str) -> EcpResponse {
    EcpResponse {
        status,
        headers: Vec::new(),
        body: body.to_owned(),
    }
}

pub fn store_with_device(
    host: &str,
    port: u16,
    access_tier: &str,
) -> (tempfile::TempDir, Arc<Mutex<SqliteStore>>) {
    let directory = tempfile::tempdir().expect("create store directory");
    let store = SqliteStore::open(&directory.path().join("api.db")).expect("open store");
    store
        .upsert_device(&DeviceRow {
            id: zestroku_core::tier::device_id(host, port),
            name: "Fixture Roku".into(),
            ip: host.into(),
            model: "Roku Fixture".into(),
            model_num: "FIXTURE1".into(),
            firmware: "15.2.4".into(),
            wifi_driver: "fixture".into(),
            access_tier: access_tier.into(),
            last_seen: 1_700_000_000,
        })
        .expect("seed device");
    store
        .upsert_access_tier(&AccessTierRow {
            device_id: zestroku_core::tier::device_id(host, port),
            tier: access_tier.into(),
            probe_at: 1_700_000_000,
            probe_map: "{}".into(),
        })
        .expect("seed recorded tier");
    (directory, Arc::new(Mutex::new(store)))
}

pub fn apps_path(host: &str, port: u16) -> String {
    format!("/v1/devices/{host}:{port}/apps")
}

pub fn error_code(response: &fastapi::TestResponse) -> String {
    assert_no_cors(response);
    let body: Value = serde_json::from_str(response.text()).expect("error response JSON");
    body["error"].as_str().expect("error code").to_owned()
}
pub fn assert_no_cors(response: &fastapi::TestResponse) {
    assert!(
        response.header("access-control-allow-origin").is_none(),
        "HTTP API responses must not grant cross-origin read access"
    );
}

pub fn resolve_schema<'a>(spec: &'a Value, schema: &'a Value) -> &'a Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(reference) => {
            let name = reference
                .strip_prefix("#/components/schemas/")
                .expect("local component schema reference");
            &spec["components"]["schemas"][name]
        }
        None => schema,
    }
}
