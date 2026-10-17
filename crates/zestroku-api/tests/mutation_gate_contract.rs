#![forbid(unsafe_code)]

#[path = "common/mod.rs"]
mod common;
use common::*;

use std::sync::Arc;

use fastapi::TestClient;
use serde_json::Value;
use zestroku_api::build_app;
use zestroku_core::store::Store;
use zestroku_proto::Method as EcpMethod;
use zestroku_types::AccessTier;

#[test]
fn tier_refusal_precedes_http_token_consumption_and_snapshot_is_audited() {
    use zestroku_core::mutation::issue_approval;
    use zestroku_core::store::AccessTierRow;
    use zestroku_proto::launch;
    use zestroku_types::{AccessTier, Device};

    let (_directory, store) = store_with_device(HOST, PORT, "limited");
    let device = Device {
        host: HOST.into(),
        port: PORT,
        model_name: "Roku Fixture".into(),
        model_number: "FIXTURE1".into(),
        software_version: "15.2.4".into(),
        is_tv: false,
        access_tier: AccessTier::Permissive,
    };
    let token = {
        let store_guard = store.lock().expect("store lock");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_secs() as i64;
        issue_approval(
            &*store_guard,
            &device,
            &launch(1001),
            zestroku_core::mutation::ClientPrincipal::LoopbackHttp,
            now,
        )
        .expect("mint launch token")
    };
    let transport = Arc::new(ResponseTransport::new([
        response(200, "<active-app><app>Roku</app></active-app>"),
        response(200, "<active-app><app>Roku</app></active-app>"),
        response(200, ""),
    ]));
    let client = TestClient::new(build_app(Arc::clone(&store), Arc::clone(&transport)));
    let path = format!("/v1/devices/{HOST}:{PORT}/launch");

    let refused = client
        .post(&path)
        .header_str("host", HOST)
        .json(&serde_json::json!({
            "client": "loopback-http",
            "app_id": 1001,
            "confirm": &token
        }))
        .send();
    assert_eq!(refused.status().as_u16(), 403);
    assert_eq!(error_code(&refused), "tier_refused");
    let first_calls = transport.calls();
    assert_eq!(
        first_calls.len(),
        1,
        "tier denial may snapshot but never POST"
    );
    assert_eq!(first_calls[0].2.method, EcpMethod::Get);
    assert_eq!(first_calls[0].2.path, "/query/active-app");

    {
        let store_guard = store.lock().expect("store lock");
        let mut row = store_guard
            .get_device(&zestroku_core::tier::device_id(HOST, PORT))
            .expect("lookup device")
            .expect("seeded device");
        row.access_tier = "permissive".into();
        store_guard
            .upsert_device(&row)
            .expect("promote fixture tier");
        store_guard
            .upsert_access_tier(&AccessTierRow {
                device_id: zestroku_core::tier::device_id(HOST, PORT),
                tier: "permissive".into(),
                probe_at: 2,
                probe_map: r#"{"query/apps":"open"}"#.into(),
            })
            .expect("promote recorded tier");
    }

    let approved = client
        .post(&path)
        .header_str("host", HOST)
        .json(&serde_json::json!({
            "client": "loopback-http",
            "app_id": 1001,
            "confirm": &token
        }))
        .send();
    assert_eq!(approved.status().as_u16(), 200);
    let approved_json: Value = serde_json::from_str(approved.text()).expect("success JSON");
    assert_eq!(approved_json["target"], format!("{HOST}:{PORT}"));
    assert_eq!(approved_json["surface"], "launch");
    assert_eq!(approved_json["outcome"], "completed");
    assert!(!approved.text().contains(&token));

    let replay = client
        .post(&path)
        .header_str("host", HOST)
        .json(&serde_json::json!({
            "client": "loopback-http",
            "app_id": 1001,
            "confirm": &token
        }))
        .send();
    assert_eq!(replay.status().as_u16(), 403);
    assert_eq!(error_code(&replay), "confirmation_required");

    let calls = transport.calls();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0].2.method, EcpMethod::Get);
    assert_eq!(calls[1].2.method, EcpMethod::Get);
    assert_eq!(calls[2].2.method, EcpMethod::Post);
    assert_eq!(calls[3].2.method, EcpMethod::Get);
    assert_eq!(calls[0].2.path, "/query/active-app");
    assert_eq!(calls[1].2.path, "/query/active-app");
    assert_eq!(calls[2].2.path, "/launch/1001");
    assert_eq!(calls[3].2.path, "/query/active-app");

    let store_guard = store.lock().expect("store lock");
    let actions = store_guard.list_actions().expect("list actions");
    assert_eq!(actions.len(), 3);
    assert_eq!(actions[0].decision, "denied");
    assert_eq!(actions[0].result, zestroku_core::mutation::TIER_REFUSED);
    assert_eq!(actions[1].decision, "approved");
    assert_eq!(actions[1].client, "loopback-http");
    assert_eq!(actions[1].surface, "launch");
    assert_eq!(
        actions[1].intent,
        format!("launch {HOST}:{PORT} /launch/1001")
    );
    assert_eq!(
        actions[1].before_state,
        zestroku_core::tier::marked_before_app_state(
            "recorded",
            &zestroku_core::tier::device_id(HOST, PORT),
            None,
            "Roku",
        ),
    );
    assert_eq!(actions[1].result, "completed");
    assert_eq!(approved_json["action_id"].as_i64(), Some(actions[1].id));
    assert_eq!(actions[2].decision, "denied");
    assert_eq!(
        actions[2].result,
        zestroku_core::mutation::CONFIRMATION_INVALID
    );
    assert!(actions.iter().all(|action| !action.intent.contains(&token)));
}

#[test]
fn unknown_inventory_and_malformed_mutation_inputs_make_no_device_requests() {
    let (_directory, store) = store_with_device(HOST, PORT, "permissive");
    let transport = Arc::new(ResponseTransport::new([]));
    let client = TestClient::new(build_app(store, Arc::clone(&transport)));

    let unknown = client
        .post("/v1/devices/192.0.2.99:18063/launch")
        .header_str("host", HOST)
        .json(&serde_json::json!({"app_id": 1001}))
        .send();
    assert_eq!(unknown.status().as_u16(), 404);
    assert_eq!(error_code(&unknown), "device_not_found");
    assert!(
        transport.calls().is_empty(),
        "unknown inventory is rejected before snapshot"
    );

    let injected = client
        .post(&format!("/v1/devices/{HOST}:{PORT}/launch"))
        .header_str("host", HOST)
        .json(&serde_json::json!({"app_id": 1001, "host": "192.0.2.99"}))
        .send();
    assert_eq!(injected.status().as_u16(), 400);
    assert_eq!(error_code(&injected), "invalid_json");
    assert!(
        transport.calls().is_empty(),
        "body overrides are rejected before ECP I/O"
    );

    let invalid_key = client
        .post(&format!("/v1/devices/{HOST}:{PORT}/key"))
        .header_str("host", HOST)
        .json(&serde_json::json!({
            "client": "loopback-http",
            "key": "Home/../../host",
            "confirm": "token"
        }))
        .send();
    assert_eq!(invalid_key.status().as_u16(), 400);
    assert_eq!(error_code(&invalid_key), "invalid_key");
    assert!(
        transport.calls().is_empty(),
        "invalid key path segment is rejected before ECP I/O"
    );
}
#[test]
fn http_approval_requires_nonblank_single_use_device_and_command_binding() {
    use zestroku_core::mutation::{
        APPROVAL_TTL_SECS, CONFIRMATION_INVALID, CONFIRMATION_REQUIRED, issue_approval,
    };
    use zestroku_proto::{keypress_raw, launch};
    use zestroku_types::Device;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64;
    let device = Device {
        host: HOST.into(),
        port: PORT,
        model_name: "Roku Fixture".into(),
        model_number: "FIXTURE1".into(),
        software_version: "15.2.4".into(),
        is_tv: false,
        access_tier: AccessTier::Permissive,
    };

    for case in [
        "missing",
        "blank",
        "free-text",
        "wrong-action",
        "wrong-device",
        "expired",
    ] {
        let (_directory, store) = store_with_device(HOST, PORT, "permissive");
        let token = {
            let store_guard = store.lock().expect("store lock");
            match case {
                "missing" => None,
                "blank" => Some("  ".to_owned()),
                "free-text" => Some("yes".to_owned()),
                "wrong-action" => Some(
                    issue_approval(
                        &*store_guard,
                        &device,
                        &keypress_raw("Home"),
                        zestroku_core::mutation::ClientPrincipal::LoopbackHttp,
                        now,
                    )
                    .expect("mint wrong-action approval"),
                ),
                "wrong-device" => Some(
                    issue_approval(
                        &*store_guard,
                        &Device {
                            host: "192.0.2.99".into(),
                            ..device.clone()
                        },
                        &launch(1001),
                        zestroku_core::mutation::ClientPrincipal::LoopbackHttp,
                        now,
                    )
                    .expect("mint wrong-device approval"),
                ),
                "expired" => Some(
                    issue_approval(
                        &*store_guard,
                        &device,
                        &launch(1001),
                        zestroku_core::mutation::ClientPrincipal::LoopbackHttp,
                        now - APPROVAL_TTL_SECS - 1,
                    )
                    .expect("mint expired approval"),
                ),
                _ => unreachable!("test case is enumerated"),
            }
        };
        let body = match token.as_deref() {
            Some(token) => serde_json::json!({
                "client": "loopback-http",
                "app_id": 1001,
                "confirm": token
            }),
            None => serde_json::json!({"client": "loopback-http", "app_id": 1001}),
        };
        let transport = Arc::new(ResponseTransport::new([response(
            200,
            "<active-app><app>Roku</app></active-app>",
        )]));
        let client = TestClient::new(build_app(Arc::clone(&store), Arc::clone(&transport)));
        let denied = client
            .post(&format!("/v1/devices/{HOST}:{PORT}/launch"))
            .header_str("host", HOST)
            .json(&body)
            .send();
        assert_eq!(denied.status().as_u16(), 403, "case={case}");
        assert_eq!(error_code(&denied), "confirmation_required", "case={case}");
        let response_body = denied.text();
        if let Some(token) = token.as_deref() {
            assert!(
                !response_body.contains(token),
                "response leaked token: {case}"
            );
        }
        let calls = transport.calls();
        assert_eq!(
            calls.len(),
            1,
            "denied request may snapshot once only: {case}"
        );
        assert_eq!(calls[0].2.method, EcpMethod::Get, "case={case}");
        assert_eq!(calls[0].2.path, "/query/active-app", "case={case}");
        let store_guard = store.lock().expect("store lock");
        let actions = store_guard.list_actions().expect("action audit");
        assert_eq!(actions.len(), 1, "case={case}");
        assert_eq!(actions[0].decision, "denied", "case={case}");
        let expected = if case == "missing" || case == "blank" {
            CONFIRMATION_REQUIRED
        } else {
            CONFIRMATION_INVALID
        };
        assert_eq!(actions[0].result, expected, "case={case}");
        if let Some(token) = token {
            assert!(
                !actions[0].intent.contains(&token),
                "audit leaked token: {case}"
            );
            assert!(
                !actions[0].before_state.contains(&token),
                "audit leaked token: {case}"
            );
        }
    }
}
#[test]
fn open_and_unknown_http_tiers_deny_before_mutation_post() {
    for tier in ["open", "unknown"] {
        let (_directory, store) = store_with_device(HOST, PORT, tier);
        let transport = Arc::new(ResponseTransport::new([response(
            200,
            "<active-app><app>Roku</app></active-app>",
        )]));
        let client = TestClient::new(build_app(Arc::clone(&store), Arc::clone(&transport)));
        let denied = client
            .post(&format!("/v1/devices/{HOST}:{PORT}/launch"))
            .header_str("host", HOST)
            .json(&serde_json::json!({"app_id": 1001}))
            .send();
        assert_eq!(denied.status().as_u16(), 403, "tier={tier}");
        assert_eq!(error_code(&denied), "tier_refused", "tier={tier}");
        let calls = transport.calls();
        assert_eq!(calls.len(), 1, "snapshot GET only for tier={tier}");
        assert_eq!(calls[0].2.method, EcpMethod::Get);
        assert_eq!(calls[0].2.path, "/query/active-app");
        let store_guard = store.lock().expect("store lock");
        let actions = store_guard.list_actions().expect("audit rows");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].decision, "denied");
        assert_eq!(actions[0].result, zestroku_core::mutation::TIER_REFUSED);
    }
}

#[test]
fn recorded_tier_downgrade_during_snapshot_denies_without_mutation_post() {
    use std::sync::Mutex;
    use zestroku_core::mutation::issue_approval;
    use zestroku_core::store::{AccessTierRow, SqliteStore};
    use zestroku_proto::{Command, EcpResponse, Transport, launch};
    use zestroku_types::{Device, Result};

    /// A tier writer landing inside the pre-authorization snapshot await,
    /// as a concurrent `/info` probe would: the gate must observe the new
    /// tier instead of the pre-snapshot value.
    struct DowngradingTransport {
        store: Arc<std::sync::Mutex<SqliteStore>>,
        calls: Mutex<Vec<(String, u16, Command)>>,
        downgraded: Mutex<bool>,
    }

    impl DowngradingTransport {
        fn calls(&self) -> Vec<(String, u16, Command)> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl Transport for DowngradingTransport {
        async fn send(&self, host: &str, port: u16, command: &Command) -> Result<EcpResponse> {
            self.calls
                .lock()
                .expect("calls lock")
                .push((host.to_owned(), port, command.clone()));
            if command.path == "/query/active-app" && !*self.downgraded.lock().expect("flag lock") {
                *self.downgraded.lock().expect("flag lock") = true;
                self.store
                    .lock()
                    .expect("store lock")
                    .upsert_access_tier(&AccessTierRow {
                        device_id: zestroku_core::tier::device_id(host, port),
                        tier: "limited".into(),
                        probe_at: 1_700_000_001,
                        probe_map: r#"{"query/apps":"limited"}"#.into(),
                    })
                    .expect("downgrade recorded tier");
                return Ok(EcpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: "<active-app><app>Roku</app></active-app>".into(),
                });
            }
            Ok(EcpResponse {
                status: 200,
                headers: Vec::new(),
                body: String::new(),
            })
        }
    }

    let (_directory, store) = store_with_device(HOST, PORT, "permissive");
    let device = Device {
        host: HOST.into(),
        port: PORT,
        model_name: "Roku Fixture".into(),
        model_number: "FIXTURE1".into(),
        software_version: "15.2.4".into(),
        is_tv: false,
        access_tier: AccessTier::Permissive,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64;
    let token = issue_approval(
        &*store.lock().expect("store lock"),
        &device,
        &launch(1001),
        zestroku_core::mutation::ClientPrincipal::LoopbackHttp,
        now,
    )
    .expect("mint launch token");
    let transport = Arc::new(DowngradingTransport {
        store: Arc::clone(&store),
        calls: Mutex::new(Vec::new()),
        downgraded: Mutex::new(false),
    });
    let client = TestClient::new(build_app(Arc::clone(&store), Arc::clone(&transport)));
    let path = format!("/v1/devices/{HOST}:{PORT}/launch");
    let denied = client
        .post(&path)
        .header_str("host", HOST)
        .json(&serde_json::json!({
            "client": "loopback-http",
            "app_id": 1001,
            "confirm": &token
        }))
        .send();
    assert_eq!(denied.status().as_u16(), 403);
    assert_eq!(error_code(&denied), "tier_refused");
    let calls = transport.calls();
    assert_eq!(calls.len(), 1, "denial precedes any mutation dispatch");
    assert_eq!(calls[0].2.method, EcpMethod::Get);
    assert_eq!(calls[0].2.path, "/query/active-app");
    // The refused token must be unconsumed: restoring the tier lets the
    // same token authorize exactly once.
    store
        .lock()
        .expect("store lock")
        .upsert_access_tier(&AccessTierRow {
            device_id: zestroku_core::tier::device_id(HOST, PORT),
            tier: "permissive".into(),
            probe_at: 1_700_000_002,
            probe_map: r#"{"query/apps":"open"}"#.into(),
        })
        .expect("restore recorded tier");
    let approved = client
        .post(&path)
        .header_str("host", HOST)
        .json(&serde_json::json!({
            "client": "loopback-http",
            "app_id": 1001,
            "confirm": &token
        }))
        .send();
    assert_eq!(approved.status().as_u16(), 200);
    let posts: Vec<String> = transport
        .calls()
        .into_iter()
        .filter(|(_, _, command)| command.method == EcpMethod::Post)
        .map(|(_, _, command)| command.path)
        .collect();
    assert_eq!(posts, ["/launch/1001"]);
}
