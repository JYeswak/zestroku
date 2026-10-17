#![forbid(unsafe_code)]

// HTTP mutation behavior using the real serve process and loopback simulator.

#[path = "common/mutation_daemon.rs"]
mod mutation_daemon;

use mutation_daemon::{http_free_port, mint_http_approval, seed_http_device, start_http_daemon};

fn post_json_to_daemon(
    port: u16,
    path: &str,
    host_header: &str,
    body: &serde_json::Value,
) -> (u16, String, String) {
    let body = serde_json::to_string(body).expect("serialize HTTP JSON request");
    post_http_request_to_daemon(port, path, host_header, None, "application/json", &body)
}

fn post_http_request_to_daemon(
    port: u16,
    path: &str,
    host_header: &str,
    origin: Option<&str>,
    content_type: &str,
    body: &str,
) -> (u16, String, String) {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to serve API");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set response timeout");
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {host_header}\r\nContent-Type: {content_type}\r\n"
    )
    .expect("write HTTP request headers");
    if let Some(origin) = origin {
        write!(stream, "Origin: {origin}\r\n").expect("write Origin header");
    }
    write!(
        stream,
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .expect("finish HTTP request headers");
    stream
        .write_all(body.as_bytes())
        .expect("write HTTP request body");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("read HTTP response");
    let response = String::from_utf8(response).expect("HTTP response UTF-8");
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .expect("HTTP header terminator");
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .expect("HTTP status code");
    (status, headers.to_owned(), body.to_owned())
}

fn assert_no_http_mutation_side_effects(db: &std::path::Path, sim: &zestroku_sim::SimDevice) {
    use zestroku_core::store::{SqliteStore, Store};

    assert!(sim.received_requests().is_empty(), "guard reached ECP");
    assert!(
        SqliteStore::open(db)
            .expect("open guard audit")
            .list_actions()
            .expect("list actions")
            .is_empty(),
        "guard wrote an audit action"
    );
}

fn assert_no_cors_header(headers: &str) {
    assert!(
        !headers
            .to_ascii_lowercase()
            .contains("access-control-allow-origin")
    );
}

#[test]
fn real_http_mutations_use_inventory_bound_approval_snapshot_and_audit() {
    use zestroku_core::store::{SqliteStore, Store};
    use zestroku_proto::{keypress_raw, launch};
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let mut sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("temporary mutation database");
    let db = directory.path().join("actions.db");
    let device_id = seed_http_device(&db, sim.port());

    let launch_token = mint_http_approval(&db, sim.port(), &launch(1001));
    let key_token = mint_http_approval(&db, sim.port(), &keypress_raw("Home"));
    let failure_token = mint_http_approval(&db, sim.port(), &launch(1002));
    let api_port = http_free_port();
    let daemon = start_http_daemon(&db, api_port);
    let target = format!("127.0.0.1:{}", sim.port());
    let api_host = format!("127.0.0.1:{api_port}");
    let launch_path = format!("/v1/devices/{target}/launch");
    let launch_body_json =
        serde_json::json!({"app_id": 1001, "confirm": &launch_token}).to_string();
    let malformed_json = format!(r#"{{"app_id":1001,"confirm":"{launch_token}",broken}}"#);
    let (origin_status, origin_headers, origin_body) = post_http_request_to_daemon(
        api_port,
        &launch_path,
        &api_host,
        Some("http://attacker.example"),
        "application/json",
        &malformed_json,
    );
    assert_eq!(origin_status, 403, "{origin_body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&origin_body).unwrap()["error"],
        "UNTRUSTED_ORIGIN"
    );
    assert!(!origin_body.contains(&launch_token));
    assert_no_cors_header(&origin_headers);
    assert_no_http_mutation_side_effects(&db, &sim);

    let (media_status, media_headers, media_body) = post_http_request_to_daemon(
        api_port,
        &launch_path,
        &api_host,
        None,
        "text/plain",
        &launch_body_json,
    );
    assert_eq!(media_status, 415, "{media_body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&media_body).unwrap()["error"],
        "unsupported_media_type"
    );
    assert!(!media_body.contains(&launch_token));
    assert_no_cors_header(&media_headers);
    assert_no_http_mutation_side_effects(&db, &sim);

    let (host_status, host_headers, host_body) = post_http_request_to_daemon(
        api_port,
        &launch_path,
        "attacker.example",
        None,
        "application/json",
        &launch_body_json,
    );
    assert!(
        matches!(host_status, 400 | 421),
        "{host_status} {host_body}"
    );
    assert!(!host_body.contains(&launch_token));
    assert_no_cors_header(&host_headers);
    assert_no_http_mutation_side_effects(&db, &sim);

    let mut case_variant = launch_token.clone();
    let letter_index = case_variant
        .bytes()
        .position(|byte| byte.is_ascii_alphabetic())
        .expect("hex approval token contains an alphabetic digit");
    let letter = case_variant.as_bytes()[letter_index] as char;
    let replacement = if letter.is_ascii_lowercase() {
        letter.to_ascii_uppercase()
    } else {
        letter.to_ascii_lowercase()
    };
    case_variant.replace_range(letter_index..letter_index + 1, &replacement.to_string());
    for requested_app in [
        " ".to_owned(),
        format!("label:{launch_token}"),
        format!("label:{case_variant}"),
    ] {
        let (status, headers, body) = post_json_to_daemon(
            api_port,
            &launch_path,
            &api_host,
            &serde_json::json!({
                "app_id": 1001,
                "confirm": &launch_token,
                "requested_app": requested_app,
            }),
        );
        assert_eq!(status, 400);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["error"],
            "invalid_requested_app"
        );
        assert!(
            !headers
                .to_ascii_lowercase()
                .contains(&launch_token.to_ascii_lowercase())
        );
        assert!(
            !body
                .to_ascii_lowercase()
                .contains(&launch_token.to_ascii_lowercase())
        );
        assert_no_http_mutation_side_effects(&db, &sim);
    }

    let same_origin = format!("http://{api_host}");
    let (launch_status, launch_headers, launch_body) = post_http_request_to_daemon(
        api_port,
        &launch_path,
        &api_host,
        Some(&same_origin),
        "application/json",
        &launch_body_json,
    );
    assert!(
        !launch_headers
            .to_ascii_lowercase()
            .contains("access-control-allow-origin")
    );
    assert!(
        !launch_headers.contains(&launch_token),
        "HTTP response headers must not expose the approval token"
    );
    assert_eq!(launch_status, 200, "{launch_body}");
    let launch_response: serde_json::Value =
        serde_json::from_str(&launch_body).expect("launch response JSON");
    assert_eq!(launch_response["target"], target);
    assert_eq!(launch_response["surface"], "launch");
    assert_eq!(launch_response["outcome"], "completed");

    let (key_status, key_headers, key_body) = post_json_to_daemon(
        api_port,
        &format!("/v1/devices/{target}/key"),
        &api_host,
        &serde_json::json!({"key": "Home", "confirm": &key_token}),
    );
    assert!(
        !key_headers
            .to_ascii_lowercase()
            .contains("access-control-allow-origin")
    );
    assert!(
        !key_headers.contains(&key_token),
        "HTTP response headers must not expose the approval token"
    );
    assert_eq!(key_status, 200, "{key_body}");
    let key_response: serde_json::Value =
        serde_json::from_str(&key_body).expect("key response JSON");
    assert_eq!(key_response["target"], target);
    assert_eq!(key_response["surface"], "key");
    assert_eq!(key_response["outcome"], "completed");
    sim.suspend();
    let (failure_status, failure_headers, failure_body) = post_json_to_daemon(
        api_port,
        &format!("/v1/devices/{target}/launch"),
        &api_host,
        &serde_json::json!({"app_id": 1002, "confirm": &failure_token}),
    );
    assert!(
        !failure_headers.contains(&failure_token),
        "HTTP response headers must not expose the approval token"
    );
    assert_eq!(failure_status, 502, "{failure_body}");
    let failure_json: serde_json::Value =
        serde_json::from_str(&failure_body).expect("transport error JSON");
    assert_eq!(failure_json["error"], "transport_error");
    assert!(
        !failure_headers
            .to_ascii_lowercase()
            .contains("access-control-allow-origin")
    );

    assert_eq!(
        sim.received_requests(),
        vec![
            ("GET".into(), "/query/active-app".into()),
            ("POST".into(), "/launch/1001".into()),
            ("GET".into(), "/query/active-app".into()),
            ("POST".into(), "/keypress/Home".into()),
        ],
        "each approval dispatches exactly one matching ECP mutation after its snapshot"
    );
    assert_eq!(sim.launch_count(), 1);

    let daemon_output = daemon.stop();
    let stdout = String::from_utf8(daemon_output.stdout).expect("daemon stdout UTF-8");
    let stderr = String::from_utf8(daemon_output.stderr).expect("daemon stderr UTF-8");
    let store = SqliteStore::open(&db).expect("read action audit");
    let actions = store.list_actions().expect("list action rows");
    assert_eq!(actions.len(), 3);
    for (action, response, surface, path, before_app_id, before) in [
        (
            &actions[0],
            &launch_response,
            "launch",
            "/launch/1001",
            None,
            "Roku",
        ),
        (
            &actions[1],
            &key_response,
            "key",
            "/keypress/Home",
            Some(1001),
            "Streaming One",
        ),
    ] {
        assert_eq!(response["action_id"].as_i64(), Some(action.id));
        assert_eq!(action.client, "http");
        assert_eq!(action.surface, surface);
        assert_eq!(action.intent, format!("{surface} {device_id} {path}"));
        assert_eq!(
            action.before_state,
            zestroku_core::tier::marked_before_app_state(
                "recorded",
                &device_id,
                before_app_id,
                before
            )
        );
        assert_eq!(action.decision, "approved");
        assert_eq!(action.result, "completed");
    }
    assert_eq!(actions[2].decision, "approved");
    assert_eq!(actions[2].result, "dispatch_failed");
    assert_eq!(
        actions[2].before_state,
        zestroku_core::tier::marked_before_app_state("recorded", &device_id, None, "unreachable")
    );
    assert!(!failure_body.contains(&failure_token));
    for secret in [&launch_token, &key_token, &failure_token] {
        for response in [
            &origin_headers,
            &origin_body,
            &media_headers,
            &media_body,
            &host_headers,
            &host_body,
            &launch_headers,
            &key_headers,
            &failure_headers,
            &failure_body,
        ] {
            assert!(
                !response.contains(secret),
                "approval tokens must not enter any HTTP response header or body"
            );
        }
        assert!(!launch_body.contains(secret));
        assert!(!key_body.contains(secret));
        assert!(!stdout.contains(secret));
        assert!(!stderr.contains(secret));
        assert!(
            sim.received_requests()
                .iter()
                .all(|(_, uri)| !uri.contains(secret)),
            "approval token must not enter an ECP URI"
        );
        assert!(actions.iter().all(|action| {
            !action.client.contains(secret)
                && !action.surface.contains(secret)
                && !action.intent.contains(secret)
                && !action.before_state.contains(secret)
                && !action.result.contains(secret)
        }));
    }
}
