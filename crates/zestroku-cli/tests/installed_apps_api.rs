#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use zestroku_core::store::{DeviceRow, SqliteStore, Store};
use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener
        .local_addr()
        .expect("read ephemeral address")
        .port()
}

fn http_get(
    port: u16,
    path: &str,
    host: &str,
    read_timeout: Duration,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(read_timeout))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    let status = raw
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| std::io::Error::other("missing HTTP status line"))?;
    let body = raw
        .split_once("\r\n\r\n")
        .map_or_else(String::new, |(_, body)| body.to_owned());
    Ok((status, body))
}

/// Wait for the daemon poller to publish a fresh apps status, not a guessed delay.
/// One inventory poll interval plus one five-second loop tick bounds the wait.
fn wait_for_fresh_apps_status(port: u16, target: &str, host: &str, expected: &str) -> Value {
    let path = format!("/v1/devices/{target}/state");
    let deadline = Instant::now()
        + Duration::from_secs(zestroku_core::DEVICE_STATE_POLL_INTERVAL_SECS as u64 + 5);
    loop {
        let (status, body) =
            http_get(port, &path, host, Duration::from_secs(5)).expect("GET cached device state");
        let last_observation = match status {
            200 => {
                let state: Value = serde_json::from_str(&body).expect("device state JSON");
                if state["fresh"] == true && state["apps_status"] == expected {
                    return state;
                }
                state.to_string()
            }
            503 => body,
            other => panic!("GET cached device state returned {other}: {body}"),
        };
        assert!(
            Instant::now() < deadline,
            "daemon poller did not publish fresh apps_status={expected}; last={last_observation}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn start_daemon(db: &Path, port: u16) -> Daemon {
    let targets = db.with_extension("targets.toml");
    std::fs::write(&targets, "").expect("write empty test targets");
    let child = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .args(["serve", "--bind"])
        .arg(format!("127.0.0.1:{port}"))
        .args(["--no-ssdp", "--targets"])
        .arg(&targets)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn zestroku serve");
    let mut daemon = Daemon(child);
    let host = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        if let Some(status) = daemon.0.try_wait().expect("poll daemon process") {
            panic!("zestroku serve exited before readiness: {status}");
        }
        if matches!(
            http_get(port, "/health", &host, Duration::from_millis(250)),
            Ok((200, _))
        ) {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "serve did not answer GET /health"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn seed_sim_device(db: &Path, host: &str, port: u16) {
    let store = SqliteStore::open(db).expect("open seeded store");
    store
        .upsert_device(&DeviceRow {
            id: zestroku_core::tier::device_id(host, port),
            name: "Sim Roku".into(),
            ip: host.into(),
            model: "Sim Express".into(),
            model_num: "SIMX1".into(),
            firmware: "15.2.4".into(),
            wifi_driver: "fixture".into(),
            access_tier: "permissive".into(),
            last_seen: 1,
        })
        .expect("seed simulator device");
}

fn error_code(body: &str) -> String {
    let value: Value = serde_json::from_str(body).expect("error response JSON");
    value["error"].as_str().expect("error code").to_owned()
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
fn real_serve_reads_and_documents_simulator_installed_apps() {
    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let sim_host = "127.0.0.1";
    let sim_port = sim.port();
    let target = format!("{sim_host}:{sim_port}");
    let directory = tempfile::tempdir().expect("create store directory");
    let db = directory.path().join("apps.db");
    seed_sim_device(&db, sim_host, sim_port);

    let serve_port = free_port();
    let _daemon = start_daemon(&db, serve_port);
    let service_host = format!("127.0.0.1:{serve_port}");

    let (status, info_body) = http_get(
        serve_port,
        &format!("/v1/devices/{target}/info"),
        &service_host,
        Duration::from_secs(5),
    )
    .expect("GET device info");
    assert_eq!(status, 200, "{info_body}");
    let info: Value = serde_json::from_str(&info_body).expect("device info JSON");
    assert_eq!(info["host"], sim_host);
    assert_eq!(info["model_name"], "Sim Express");
    assert_eq!(info["access_tier"], "permissive");

    let apps_path = format!("/v1/devices/{target}/apps");
    let (status, apps_body) = http_get(
        serve_port,
        &apps_path,
        &service_host,
        Duration::from_secs(5),
    )
    .expect("GET installed apps");
    assert_eq!(status, 200, "{apps_body}");
    let apps: Value = serde_json::from_str(&apps_body).expect("installed apps JSON");
    assert_eq!(apps["host"], sim_host);
    let app_rows = apps["apps"].as_array().expect("app list");
    let actual: Vec<(String, String, String)> = app_rows
        .iter()
        .map(|app| {
            (
                app["id"].as_str().expect("app id").to_owned(),
                app["name"].as_str().expect("app name").to_owned(),
                app["type"].as_str().expect("app type").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        actual,
        vec![
            ("1001".into(), "Streaming One".into(), "appl".into()),
            ("1002".into(), "Streaming Two".into(), "appl".into()),
            ("1003".into(), "Example Channel".into(), "appl".into()),
            (
                "tvinput.hdmi1".into(),
                "HDMI Input One".into(),
                "tvin".into()
            ),
            ("tvinput.dtv".into(), "Live TV Input".into(), "tvin".into()),
        ]
    );

    let (status, spec_body) = http_get(
        serve_port,
        "/openapi.json",
        &service_host,
        Duration::from_secs(5),
    )
    .expect("GET OpenAPI document");
    assert_eq!(status, 200, "{spec_body}");
    let spec: Value = serde_json::from_str(&spec_body).expect("OpenAPI JSON");
    assert_eq!(spec["openapi"], "3.1.0");
    let paths = spec["paths"].as_object().expect("documented paths");
    assert!(paths.contains_key("/v1/devices/{host}/apps"));
    let mut post_paths: Vec<_> = paths
        .iter()
        .filter_map(|(path, item)| item.get("post").map(|_| path.as_str()))
        .collect();
    post_paths.sort_unstable();
    assert_eq!(
        post_paths,
        [
            "/v1/devices/{host}/key",
            "/v1/devices/{host}/launch",
            "/v1/devices/{host}/undo",
        ],
        "only approved mutation operations are exposed"
    );
    for item in paths.values() {
        assert!(item.get("put").is_none());
        assert!(item.get("patch").is_none());
        assert!(item.get("delete").is_none());
    }
    let operation = &spec["paths"]["/v1/devices/{host}/apps"]["get"];
    let response_schema = resolve_schema(
        &spec,
        &operation["responses"]["200"]["content"]["application/json"]["schema"],
    );
    let item_schema = resolve_schema(&spec, &response_schema["properties"]["apps"]["items"]);
    for field in ["id", "name", "type"] {
        assert_eq!(
            item_schema["properties"][field]["type"], "string",
            "{field}"
        );
        assert!(app_rows.iter().all(|app| app[field].is_string()), "{field}");
    }

    let initial_state = wait_for_fresh_apps_status(serve_port, &target, &service_host, "available");
    assert_eq!(initial_state["access_tier"], "permissive");
    let app_queries_before_tier_change = sim
        .received_requests()
        .iter()
        .filter(|(_, uri)| uri.as_str() == "/query/apps")
        .count();

    sim.set_tier(SimTier::Limited);
    let limited_state =
        wait_for_fresh_apps_status(serve_port, &target, &service_host, "tier_refused");
    assert_eq!(limited_state["access_tier"], "limited");
    let app_queries_after_tier_change = sim
        .received_requests()
        .iter()
        .filter(|(_, uri)| uri.as_str() == "/query/apps")
        .count();
    assert!(
        app_queries_after_tier_change > app_queries_before_tier_change,
        "poller did not query apps after the tier changed"
    );

    // Once the poller publishes the refusal, the fresh API response must not probe again.
    let previous_requests = sim.received_requests().len();
    let (status, limited_body) = http_get(
        serve_port,
        &apps_path,
        &service_host,
        Duration::from_secs(5),
    )
    .expect("GET apps after Limited tier was observed");
    assert_eq!(status, 403, "{limited_body}");
    assert_eq!(error_code(&limited_body), "tier_refused");
    assert_eq!(
        sim.received_requests().len(),
        previous_requests,
        "fresh cached tier refusal must not make another ECP request"
    );

    let previous_requests = sim.received_requests().len();
    let unknown_target = format!("/v1/devices/192.0.2.99:{sim_port}/apps");
    let (status, unknown_body) = http_get(
        serve_port,
        &unknown_target,
        &service_host,
        Duration::from_secs(5),
    )
    .expect("GET unknown device apps");
    assert_eq!(status, 404, "{unknown_body}");
    assert_eq!(error_code(&unknown_body), "device_not_found");
    assert_eq!(sim.received_requests().len(), previous_requests);
}
