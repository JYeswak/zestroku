#![forbid(unsafe_code)]

// HTTP mutation behavior using the real serve process and loopback simulator.

struct HttpApiDaemon(Option<std::process::Child>);

impl Drop for HttpApiDaemon {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl HttpApiDaemon {
    fn stop(mut self) -> std::process::Output {
        let mut child = self.0.take().expect("daemon child");
        let _ = child.kill();
        child.wait_with_output().expect("collect daemon output")
    }
}

fn http_free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("reserve HTTP port")
        .local_addr()
        .expect("HTTP port address")
        .port()
}

fn wait_for_http_daemon(port: u16) {
    use std::net::TcpStream;
    use std::thread;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "serve did not bind 127.0.0.1:{port}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

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

fn mint_http_approval(
    db: &std::path::Path,
    port: u16,
    command: &zestroku_proto::Command,
) -> String {
    use zestroku_core::mutation::issue_approval;
    use zestroku_core::store::SqliteStore;
    use zestroku_types::{AccessTier, Device};

    let store = SqliteStore::open(db).expect("open store to mint HTTP approval");
    issue_approval(
        &store,
        &Device {
            host: "127.0.0.1".into(),
            port,
            model_name: "Sim Express".into(),
            model_number: "SIMX1".into(),
            software_version: "15.2.4".into(),
            is_tv: false,
            access_tier: AccessTier::Permissive,
        },
        command,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_secs() as i64,
    )
    .expect("mint one-use HTTP approval")
}

#[test]
fn real_http_mutations_use_inventory_bound_approval_snapshot_and_audit() {
    use std::process::{Command as ProcessCommand, Stdio};

    use zestroku_core::store::{AccessTierRow, DeviceRow, SqliteStore, Store};
    use zestroku_proto::{keypress_raw, launch};
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let mut sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("temporary mutation database");
    let db = directory.path().join("actions.db");
    let device_id = zestroku_core::tier::device_id("127.0.0.1", sim.port());
    {
        let store = SqliteStore::open(&db).expect("open seed store");
        store
            .upsert_device(&DeviceRow {
                id: device_id.clone(),
                name: "Sim Express".into(),
                ip: "127.0.0.1".into(),
                model: "Sim Express".into(),
                model_num: "SIMX1".into(),
                firmware: "15.2.4".into(),
                wifi_driver: "loopback".into(),
                access_tier: "permissive".into(),
                last_seen: 1,
            })
            .expect("seed inventory device");
        store
            .upsert_access_tier(&AccessTierRow {
                device_id: device_id.clone(),
                tier: "permissive".into(),
                probe_at: 1,
                probe_map: r#"{"query/apps":"open"}"#.into(),
            })
            .expect("seed recorded tier");
    }

    let launch_token = mint_http_approval(&db, sim.port(), &launch(1001));
    let key_token = mint_http_approval(&db, sim.port(), &keypress_raw("Home"));
    let failure_token = mint_http_approval(&db, sim.port(), &launch(1002));
    let api_port = http_free_port();
    let child = ProcessCommand::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .args(["serve", "--bind", &format!("127.0.0.1:{api_port}")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start real daemon");
    let daemon = HttpApiDaemon(Some(child));
    wait_for_http_daemon(api_port);
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
    for (action, response, surface, path, before) in [
        (
            &actions[0],
            &launch_response,
            "launch",
            "/launch/1001",
            "Roku",
        ),
        (
            &actions[1],
            &key_response,
            "key",
            "/keypress/Home",
            "Streaming One",
        ),
    ] {
        assert_eq!(response["action_id"].as_i64(), Some(action.id));
        assert_eq!(action.client, "http");
        assert_eq!(action.surface, surface);
        assert_eq!(action.intent, format!("{surface} {device_id} {path}"));
        assert_eq!(
            action.before_state,
            format!("tier_source=recorded; {before}")
        );
        assert_eq!(action.decision, "approved");
        assert_eq!(action.result, "completed");
    }
    assert_eq!(actions[2].decision, "approved");
    assert_eq!(actions[2].result, "dispatch_failed");
    assert_eq!(actions[2].before_state, "tier_source=recorded; unreachable");
    assert!(!failure_body.contains(&failure_token));
    for secret in [&launch_token, &key_token, &failure_token] {
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
