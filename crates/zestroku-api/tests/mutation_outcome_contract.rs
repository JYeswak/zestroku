#![forbid(unsafe_code)]

#[path = "common/mod.rs"]
mod common;
use common::*;

use std::sync::Arc;

use fastapi::TestClient;
use zestroku_api::build_app;
use zestroku_core::store::Store;
use zestroku_proto::Method as EcpMethod;
use zestroku_types::AccessTier;

#[test]
fn approved_http_dispatch_failures_finish_as_failures() {
    use zestroku_core::mutation::issue_approval;
    use zestroku_proto::launch;
    use zestroku_types::Device;

    for absent in [false, true] {
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
            .expect("mint approval")
        };
        let mut responses = vec![response(200, "<active-app><app>Roku</app></active-app>")];
        if absent {
            responses.push(response(404, ""));
        }
        let transport = Arc::new(ResponseTransport::new(responses));
        let client = TestClient::new(build_app(Arc::clone(&store), Arc::clone(&transport)));
        let result = client
            .post(&format!("/v1/devices/{HOST}:{PORT}/launch"))
            .header_str("host", HOST)
            .json(&serde_json::json!({
                "client": "loopback-http",
                "app_id": 1001,
                "confirm": &token
            }))
            .send();
        let expected_status = if absent { 404 } else { 502 };
        assert_eq!(result.status().as_u16(), expected_status, "absent={absent}");
        assert_eq!(
            error_code(&result),
            if absent {
                "app_not_installed"
            } else {
                "transport_error"
            },
            "absent={absent}"
        );
        let store_guard = store.lock().expect("store lock");
        let actions = store_guard.list_actions().expect("action audit");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].decision, "approved");
        assert_eq!(
            actions[0].result,
            if absent {
                "app_not_installed"
            } else {
                "dispatch_failed"
            }
        );
        assert_ne!(actions[0].result, "completed");
        assert!(!result.text().contains(&token));
        let calls = transport.calls();
        assert_eq!(
            calls.len(),
            2,
            "snapshot GET plus exactly one mutation attempt"
        );
        assert_eq!(calls[0].2.method, EcpMethod::Get);
        assert_eq!(calls[0].2.path, "/query/active-app");
        assert_eq!(calls[1].2.method, EcpMethod::Post);
        assert_eq!(calls[1].2.path, "/launch/1001");
    }
}
#[test]
fn foreign_origin_and_non_json_posts_never_consume_confirmation() {
    use zestroku_core::mutation::issue_approval;
    use zestroku_proto::launch;
    use zestroku_types::Device;

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
        .expect("mint approval")
    };
    let transport = Arc::new(ResponseTransport::new([
        response(200, "<active-app><app>Roku</app></active-app>"),
        response(200, ""),
    ]));
    let client = TestClient::new(build_app(Arc::clone(&store), Arc::clone(&transport)));
    let path = format!("/v1/devices/{HOST}:{PORT}/launch");
    let authority = format!("{HOST}:{PORT}");
    let body = serde_json::json!({
        "client": "loopback-http",
        "app_id": 1001,
        "confirm": &token
    });
    let malformed_body =
        format!(r#"{{"client":"loopback-http","app_id":1001,"confirm":"{token}",broken}}"#);
    let foreign_host = client
        .post(&path)
        .header_str("host", "attacker.example")
        .header_str("origin", "http://attacker.example")
        .json(&body)
        .send();
    assert_eq!(foreign_host.status().as_u16(), 400);
    assert_eq!(error_code(&foreign_host), "invalid_host");
    assert!(!foreign_host.text().contains(&token));
    assert_no_cors(&foreign_host);

    let foreign = client
        .post(&path)
        .header_str("host", &authority)
        .header_str("origin", "http://attacker.example")
        .header_str("content-type", "application/json")
        .body_str(&malformed_body)
        .send();
    assert_eq!(foreign.status().as_u16(), 403);
    assert_eq!(error_code(&foreign), "UNTRUSTED_ORIGIN");
    assert!(!foreign.text().contains(&token));
    assert_no_cors(&foreign);

    let text_body = body.to_string();
    let wrong_media = client
        .post(&path)
        .header_str("host", &authority)
        .header_str("content-type", "text/plain")
        .body_str(&text_body)
        .send();
    assert_eq!(wrong_media.status().as_u16(), 415);
    assert_eq!(error_code(&wrong_media), "unsupported_media_type");
    assert_no_cors(&wrong_media);
    assert!(
        transport.calls().is_empty(),
        "Origin and media-type guards precede all ECP I/O"
    );
    assert!(
        store
            .lock()
            .expect("store lock")
            .list_actions()
            .expect("actions")
            .is_empty()
    );

    let same_origin = format!("http://{authority}");
    let approved = client
        .post(&path)
        .header_str("host", &authority)
        .header_str("origin", &same_origin)
        .json(&body)
        .send();
    assert_eq!(approved.status().as_u16(), 200);
    assert_no_cors(&approved);
    assert!(!approved.text().contains(&token));
    let calls = transport.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].2.path, "/query/active-app");
    assert_eq!(calls[1].2.path, "/launch/1001");
    let actions = store
        .lock()
        .expect("store lock")
        .list_actions()
        .expect("actions");
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].decision, "approved");
    assert_eq!(actions[0].result, "completed");
}

#[test]
fn browser_safety_guards_precede_reads_and_reject_dns_host_rebinding() {
    let (_directory, store) = store_with_device(HOST, PORT, "permissive");
    let transport = Arc::new(ResponseTransport::new([]));
    let client = TestClient::new(build_app(Arc::clone(&store), Arc::clone(&transport)));
    let authority = format!("{HOST}:{PORT}");

    let foreign_read = client
        .get(&format!("/v1/devices/{authority}/info"))
        .header_str("host", &authority)
        .header_str("origin", "http://attacker.example")
        .send();
    assert_eq!(foreign_read.status().as_u16(), 403);
    assert_eq!(error_code(&foreign_read), "UNTRUSTED_ORIGIN");
    assert_no_cors(&foreign_read);

    let rebinding_host = client
        .get("/health")
        .header_str("host", "attacker.example")
        .header_str("origin", "http://attacker.example")
        .send();
    assert_eq!(rebinding_host.status().as_u16(), 400);
    assert_eq!(error_code(&rebinding_host), "invalid_host");
    assert_no_cors(&rebinding_host);
    assert!(
        transport.calls().is_empty(),
        "browser safety guards precede all device queries"
    );
    assert!(
        store
            .lock()
            .expect("store lock")
            .list_actions()
            .expect("action audit")
            .is_empty(),
        "browser safety guards precede audit writes"
    );
}
