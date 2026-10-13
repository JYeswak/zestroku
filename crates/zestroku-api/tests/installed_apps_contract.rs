#![forbid(unsafe_code)]

#[path = "common/mod.rs"]
mod common;
use common::*;

use std::collections::BTreeSet;
use std::sync::Arc;

use fastapi::{OpenApi, TestClient, core::Method as HttpMethod};
use serde_json::Value;
use zestroku_api::build_app;
use zestroku_proto::Method as EcpMethod;
use zestroku_types::AccessTier;

#[test]
fn openapi_matches_registered_read_routes_and_apps_response_schema() {
    let (_directory, store) = store_with_device(HOST, PORT, "permissive");
    let transport = Arc::new(ResponseTransport::new([]));
    let app = build_app(store, transport);
    let registered: Vec<(HttpMethod, String)> = app
        .routes()
        .filter(|(_, path)| *path != "/openapi.json")
        .map(|(method, path)| (method, path.to_owned()))
        .collect();
    let expected_routes = [
        (HttpMethod::Get, "/health"),
        (HttpMethod::Get, "/v1/devices"),
        (HttpMethod::Get, "/v1/devices/{host}/active-app"),
        (HttpMethod::Get, "/v1/devices/{host}/apps"),
        (HttpMethod::Get, "/v1/devices/{host}/info"),
        (HttpMethod::Post, "/v1/devices/{host}/key"),
        (HttpMethod::Post, "/v1/devices/{host}/launch"),
    ];
    assert_eq!(registered.len(), expected_routes.len());
    for (method, path) in expected_routes {
        assert!(
            registered.contains(&(method, path.to_owned())),
            "missing {method:?} {path}"
        );
    }
    let client = TestClient::new(app);

    let response = client.get("/openapi.json").send();
    assert_eq!(response.status().as_u16(), 200);
    let spec: OpenApi = serde_json::from_str(response.text()).expect("valid OpenAPI document");
    assert_eq!(spec.openapi, "3.1.0");
    assert_eq!(spec.info.version, "v1");
    let documented: BTreeSet<String> = spec.paths.keys().cloned().collect();
    let registered_paths: BTreeSet<String> =
        registered.iter().map(|(_, path)| path.clone()).collect();
    assert_eq!(documented, registered_paths);
    assert_eq!(
        documented,
        [
            "/health",
            "/v1/devices",
            "/v1/devices/{host}/active-app",
            "/v1/devices/{host}/apps",
            "/v1/devices/{host}/info",
            "/v1/devices/{host}/key",
            "/v1/devices/{host}/launch",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );

    let spec: Value = serde_json::to_value(spec).expect("serialize OpenAPI document");
    for path in spec["paths"].as_object().expect("OpenAPI paths").values() {
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
