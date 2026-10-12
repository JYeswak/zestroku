#![forbid(unsafe_code)]

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use fastapi::core::Method as HttpMethod;
use fastapi::{OpenApi, TestClient};
use serde_json::Value;
use zestroku_api::build_app;
use zestroku_core::store::{DeviceRow, SqliteStore, Store};
use zestroku_proto::{Command, EcpResponse, Method as EcpMethod, Transport};
use zestroku_types::{AccessTier, Error, Result};

const HOST: &str = "192.0.2.10";
const PORT: u16 = 18_063;
const LIMITED_BODY: &str = "ECP command not allowed in Limited mode.";

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

fn response(status: u16, body: &str) -> EcpResponse {
    EcpResponse {
        status,
        headers: Vec::new(),
        body: body.to_owned(),
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
    (directory, Arc::new(Mutex::new(store)))
}

fn apps_path(host: &str, port: u16) -> String {
    format!("/v1/devices/{host}:{port}/apps")
}

fn error_code(response: &fastapi::TestResponse) -> String {
    let body: Value = serde_json::from_str(response.text()).expect("error response JSON");
    body["error"].as_str().expect("error code").to_owned()
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
fn openapi_matches_registered_read_routes_and_apps_response_schema() {
    let (_directory, store) = store_with_device(HOST, PORT, "permissive");
    let transport = Arc::new(ResponseTransport::new([]));
    let app = build_app(store, transport);
    let registered: BTreeSet<String> = app
        .routes()
        .filter(|(_, path)| *path != "/openapi.json")
        .map(|(method, path)| {
            assert_eq!(
                method,
                HttpMethod::Get,
                "only GET routes belong in this read API"
            );
            path.to_owned()
        })
        .collect();
    let client = TestClient::new(app);

    let response = client.get("/openapi.json").send();
    assert_eq!(response.status().as_u16(), 200);
    let spec: OpenApi = serde_json::from_str(response.text()).expect("valid OpenAPI document");
    assert_eq!(spec.openapi, "3.1.0");
    assert_eq!(spec.info.version, "v1");
    let documented: BTreeSet<String> = spec.paths.keys().cloned().collect();
    assert_eq!(documented, registered);
    assert_eq!(
        documented,
        [
            "/health",
            "/v1/devices",
            "/v1/devices/{host}/active-app",
            "/v1/devices/{host}/apps",
            "/v1/devices/{host}/info",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );

    let spec: Value = serde_json::to_value(spec).expect("serialize OpenAPI document");
    for path in spec["paths"].as_object().expect("OpenAPI paths").values() {
        assert!(
            path.get("post").is_none(),
            "no mutation routes are implemented"
        );
        assert!(path.get("put").is_none());
        assert!(path.get("patch").is_none());
        assert!(path.get("delete").is_none());
    }

    let operation = &spec["paths"]["/v1/devices/{host}/apps"]["get"];
    assert_eq!(operation["summary"], "List installed apps");
    let host_parameter = operation["parameters"]
        .as_array()
        .expect("route parameters")
        .iter()
        .find(|parameter| parameter["name"] == "host")
        .expect("host path parameter");
    assert_eq!(host_parameter["in"], "path");
    assert_eq!(host_parameter["required"], true);
    assert_eq!(host_parameter["schema"]["type"], "string");

    let schemas = &spec["components"]["schemas"];
    let apps_schema = resolve_schema(
        &spec,
        &operation["responses"]["200"]["content"]["application/json"]["schema"],
    );
    assert_eq!(apps_schema["properties"]["host"]["type"], "string");
    assert_eq!(apps_schema["properties"]["apps"]["type"], "array");
    let item_schema = resolve_schema(&spec, &apps_schema["properties"]["apps"]["items"]);
    for field in ["id", "name", "type"] {
        assert_eq!(
            item_schema["properties"][field]["type"], "string",
            "{field}"
        );
    }
    for status in ["400", "403", "404", "500", "502"] {
        let error_schema = resolve_schema(
            &spec,
            &operation["responses"][status]["content"]["application/json"]["schema"],
        );
        assert_eq!(
            error_schema["properties"]["error"]["type"], "string",
            "{status}"
        );
    }
    assert!(schemas.is_object());
}

#[test]
fn malformed_apps_xml_and_unknown_targets_have_distinct_errors() {
    let (_directory, store) = store_with_device(HOST, PORT, "permissive");
    let transport = Arc::new(ResponseTransport::new([response(200, "<apps><app")]));
    let client = TestClient::new(build_app(store, Arc::clone(&transport)));

    let malformed = client.get(&apps_path(HOST, PORT)).send();
    assert_eq!(malformed.status().as_u16(), 502);
    assert_eq!(error_code(&malformed), "malformed_apps");

    let unknown = client.get(&apps_path("192.0.2.99", PORT)).send();
    assert_eq!(unknown.status().as_u16(), 404);
    assert_eq!(error_code(&unknown), "device_not_found");

    let calls = transport.calls();
    assert_eq!(calls.len(), 1, "unknown targets must not reach ECP");
    assert_eq!(calls[0].0, HOST);
    assert_eq!(calls[0].1, PORT);
    assert_eq!(calls[0].2.path, "/query/apps");
    assert_eq!(calls[0].2.method, EcpMethod::Get);
    assert_eq!(calls[0].2.tier, AccessTier::Permissive);
}

#[test]
fn apps_route_keeps_limited_and_absent_endpoint_responses_distinct() {
    let (_limited_directory, limited_store) = store_with_device(HOST, PORT, "limited");
    let local_transport = Arc::new(ResponseTransport::new([]));
    let local_client = TestClient::new(build_app(limited_store, Arc::clone(&local_transport)));
    let local_refusal = local_client.get(&apps_path(HOST, PORT)).send();
    assert_eq!(local_refusal.status().as_u16(), 403);
    assert_eq!(error_code(&local_refusal), "tier_refused");
    assert!(
        local_transport.calls().is_empty(),
        "stored Limited tier refuses locally"
    );

    let (_limited_reply_directory, limited_reply_store) =
        store_with_device(HOST, PORT, "permissive");
    let limited_reply = Arc::new(ResponseTransport::new([response(403, LIMITED_BODY)]));
    let limited_reply_client =
        TestClient::new(build_app(limited_reply_store, Arc::clone(&limited_reply)));
    let limited = limited_reply_client.get(&apps_path(HOST, PORT)).send();
    assert_eq!(limited.status().as_u16(), 403);
    assert_eq!(error_code(&limited), "tier_refused");
    assert_eq!(limited_reply.calls().len(), 1);

    let (_absent_directory, absent_store) = store_with_device(HOST, PORT, "permissive");
    let absent_transport = Arc::new(ResponseTransport::new([response(404, "")]));
    let absent_client = TestClient::new(build_app(absent_store, Arc::clone(&absent_transport)));
    let absent = absent_client.get(&apps_path(HOST, PORT)).send();
    assert_eq!(absent.status().as_u16(), 404);
    assert_eq!(error_code(&absent), "apps_endpoint_not_found");
    assert_eq!(absent_transport.calls().len(), 1);
}
