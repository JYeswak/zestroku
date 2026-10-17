#![forbid(unsafe_code)]

//! Local fixture mirror: this target exercises routes/OpenAPI only, so it
//! must not compile the ECP-mutation helpers in common/mod.rs (`calls`,
//! `response`, `error_code`, `assert_no_cors`), which other targets consume.
//! The items below mirror common exactly; keep them in sync if it changes.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use fastapi::{TestClient, core::Method as HttpMethod};
use serde_json::Value;
use zestroku_api::build_app;
use zestroku_core::store::{AccessTierRow, DeviceRow, SqliteStore, Store};
use zestroku_proto::{Command, EcpResponse, Transport};
use zestroku_types::{Error, Result};

const HOST: &str = "192.0.2.10";
const PORT: u16 = 18_063;

struct ResponseTransport {
    responses: Mutex<VecDeque<EcpResponse>>,
    calls: Mutex<Vec<(String, u16, Command)>>,
}

impl ResponseTransport {
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

fn store_with_device(
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

fn resolve_schema<'a>(spec: &'a Value, schema: &'a Value) -> &'a Value {
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

#[test]
fn mutation_routes_document_closed_request_bodies() {
    let (_directory, store) = store_with_device(HOST, PORT, "permissive");
    let transport = Arc::new(ResponseTransport::new([]));
    let app = build_app(store, Arc::clone(&transport));
    let routes: Vec<_> = app.routes().collect();
    for (method, path) in [
        (HttpMethod::Post, "/v1/devices/{host}/key"),
        (HttpMethod::Post, "/v1/devices/{host}/launch"),
        (HttpMethod::Post, "/v1/devices/{host}/undo"),
    ] {
        assert!(
            routes.contains(&(method, path)),
            "missing {method:?} {path}"
        );
    }

    let client = TestClient::new(app);
    let response = client.get("/openapi.json").send();
    assert_eq!(response.status().as_u16(), 200);
    let spec: Value = serde_json::from_str(response.text()).expect("OpenAPI JSON");
    let key = &spec["paths"]["/v1/devices/{host}/key"]["post"];
    let launch = &spec["paths"]["/v1/devices/{host}/launch"]["post"];
    let undo = &spec["paths"]["/v1/devices/{host}/undo"]["post"];
    assert_eq!(key["requestBody"]["required"], true);
    assert_eq!(launch["requestBody"]["required"], true);
    assert_eq!(undo["requestBody"]["required"], true);
    let schemas = &spec["components"]["schemas"];
    let key_schema = resolve_schema(
        &spec,
        &key["requestBody"]["content"]["application/json"]["schema"],
    );
    assert_eq!(key_schema["type"], "object");
    assert_eq!(key_schema["additionalProperties"], false);
    assert_eq!(key_schema["required"], serde_json::json!(["key"]));
    let launch_schema = resolve_schema(
        &spec,
        &launch["requestBody"]["content"]["application/json"]["schema"],
    );
    assert_eq!(launch_schema["type"], "object");
    assert_eq!(launch_schema["additionalProperties"], false);
    assert_eq!(launch_schema["required"], serde_json::json!(["app_id"]));
    assert_eq!(key_schema["properties"]["key"]["type"], "string");
    assert_eq!(key_schema["properties"]["client"]["type"], "string");
    assert_eq!(
        key_schema["properties"]["client"]["enum"],
        serde_json::json!(["cli", "mcp-stdio", "loopback-http"])
    );
    assert!(key_schema["properties"]["confirm"].is_object());
    for forbidden in ["host", "port", "tier", "principal", "backend_url"] {
        assert!(
            key_schema["properties"].get(forbidden).is_none(),
            "key body must not supply {forbidden}"
        );
    }
    let launch_schema = resolve_schema(
        &spec,
        &launch["requestBody"]["content"]["application/json"]["schema"],
    );
    assert_eq!(launch_schema["properties"]["app_id"]["type"], "integer");
    assert_eq!(launch_schema["properties"]["client"]["type"], "string");
    assert_eq!(
        launch_schema["properties"]["client"]["enum"],
        serde_json::json!(["cli", "mcp-stdio", "loopback-http"])
    );
    assert!(launch_schema["properties"]["confirm"].is_object());
    for forbidden in ["host", "port", "tier", "principal", "backend_url"] {
        assert!(
            launch_schema["properties"].get(forbidden).is_none(),
            "launch body must not supply {forbidden}"
        );
    }
    let undo_schema = resolve_schema(
        &spec,
        &undo["requestBody"]["content"]["application/json"]["schema"],
    );
    assert_eq!(undo_schema["type"], "object");
    assert_eq!(undo_schema["additionalProperties"], false);
    assert_eq!(undo_schema["required"], serde_json::json!(["action_id"]));
    assert_eq!(undo_schema["properties"]["action_id"]["type"], "integer");
    assert_eq!(undo_schema["properties"]["client"]["type"], "string");
    assert_eq!(
        undo_schema["properties"]["client"]["enum"],
        serde_json::json!(["cli", "mcp-stdio", "loopback-http"])
    );
    assert!(undo_schema["properties"]["confirm"].is_object());
    assert!(schemas.is_object());
    for operation in [key, launch] {
        let response = &operation["responses"]["200"]["content"]["application/json"]["schema"];
        let response_schema = resolve_schema(&spec, response);
        for (field, kind) in [
            ("action_id", "integer"),
            ("target", "string"),
            ("surface", "string"),
            ("outcome", "string"),
        ] {
            assert_eq!(
                response_schema["properties"][field]["type"], kind,
                "{field}"
            );
        }
        for status in ["400", "403", "404", "413", "415", "500", "502"] {
            let error_schema = resolve_schema(
                &spec,
                &operation["responses"][status]["content"]["application/json"]["schema"],
            );
            assert_eq!(
                error_schema["properties"]["error"]["type"], "string",
                "{status}"
            );
        }
    }
    let undo_response = &undo["responses"]["200"]["content"]["application/json"]["schema"];
    let undo_response_schema = resolve_schema(&spec, undo_response);
    assert_eq!(
        undo_response_schema["properties"]["action_id"]["type"],
        "integer"
    );
    for status in ["400", "403", "404", "409", "500", "502"] {
        let error_schema = resolve_schema(
            &spec,
            &undo["responses"][status]["content"]["application/json"]["schema"],
        );
        assert_eq!(error_schema["properties"]["error"]["type"], "string");
    }
    assert!(
        transport.calls().is_empty(),
        "schema/routes-only test must not dispatch ECP"
    );
}
