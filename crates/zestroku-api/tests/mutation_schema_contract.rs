#![forbid(unsafe_code)]

#[path = "common/mod.rs"]
mod common;
use common::*;

use std::sync::Arc;

use fastapi::{TestClient, core::Method as HttpMethod};
use serde_json::Value;
use zestroku_api::build_app;

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
fn key_and_launch_mutation_routes_document_closed_request_bodies() {
    let (_directory, store) = store_with_device(HOST, PORT, "permissive");
    let app = build_app(store, Arc::new(ResponseTransport::new([])));
    let routes: Vec<_> = app.routes().collect();
    for (method, path) in [
        (HttpMethod::Post, "/v1/devices/{host}/key"),
        (HttpMethod::Post, "/v1/devices/{host}/launch"),
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
    assert_eq!(key["requestBody"]["required"], true);
    assert_eq!(launch["requestBody"]["required"], true);
    let schemas = &spec["components"]["schemas"];
    let key_schema = resolve_schema(
        &spec,
        &key["requestBody"]["content"]["application/json"]["schema"],
    );
    assert_eq!(key_schema["type"], "object");
    assert_eq!(key_schema["additionalProperties"], false);
    let launch_schema = resolve_schema(
        &spec,
        &launch["requestBody"]["content"]["application/json"]["schema"],
    );
    assert_eq!(launch_schema["type"], "object");
    assert_eq!(launch_schema["additionalProperties"], false);
    assert_eq!(key_schema["properties"]["key"]["type"], "string");
    assert!(key_schema["properties"]["confirm"].is_object());
    assert_eq!(key_schema["required"], serde_json::json!(["key"]));
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
    assert!(launch_schema["properties"]["confirm"].is_object());
    assert_eq!(launch_schema["required"], serde_json::json!(["app_id"]));
    for forbidden in ["host", "port", "tier", "principal", "backend_url"] {
        assert!(
            launch_schema["properties"].get(forbidden).is_none(),
            "launch body must not supply {forbidden}"
        );
    }
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
}
