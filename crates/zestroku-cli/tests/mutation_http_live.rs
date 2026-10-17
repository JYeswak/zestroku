//! Live HTTP key/launch through the real `serve` binary against a loopback
//! SimDevice (a0h.11.3 [a]): launch then key each execute exactly once with
//! audited rows, replays deny, unconfirmed/invalid/tier denials dispatch
//! nothing. Localhost only; approval tokens are minted in-process (HTTP
//! approval minting is out of scope).
#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use zestroku_core::store::{AccessTierRow, DeviceRow, SqliteStore, Store};

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr").port()
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(db: &std::path::Path, port: u16) -> Daemon {
    let child = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .args(["serve", "--bind", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn zestroku serve");
    let daemon = Daemon(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return daemon;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("serve did not listen on 127.0.0.1:{port} within 30s");
}

fn seed(target: &str, db: &std::path::Path) {
    let (host, port) = target
        .rsplit_once(':')
        .map(|(host, port)| (host, port.parse().expect("numeric sim port")))
        .unwrap_or((target, 8060));
    let store = SqliteStore::open(db).expect("open sim inventory");
    store
        .upsert_device(&DeviceRow {
            id: target.into(),
            name: "Sim Roku".into(),
            ip: host.into(),
            model: "Roku Express".into(),
            model_num: "SIM-1234X".into(),
            firmware: "15.2.4".into(),
            wifi_driver: "fixture".into(),
            access_tier: "permissive".into(),
            last_seen: 1,
        })
        .expect("seed sim device");
    store
        .upsert_access_tier(&AccessTierRow {
            device_id: zestroku_core::tier::device_id(host, port),
            tier: "permissive".into(),
            probe_at: 1_700_000_000,
            probe_map: "{}".into(),
        })
        .expect("seed recorded tier");
}

/// One raw HTTP/1.1 POST; returns (status, body).
fn post(port: u16, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("write");
    let mut raw = String::new();
    let _ = stream.read_to_string(&mut raw);
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
    (status, body)
}

fn mint_token(db: &std::path::Path, host: &str, port: u16, path: &str, now: i64) -> String {
    use zestroku_types::{AccessTier, Device};
    let store = SqliteStore::open(db).expect("open store for approval");
    let device = Device {
        host: host.into(),
        port,
        model_name: String::new(),
        model_number: String::new(),
        software_version: String::new(),
        is_tv: false,
        access_tier: AccessTier::Permissive,
    };
    let command = zestroku_proto::Command {
        method: zestroku_proto::Method::Post,
        path: path.into(),
        tier: AccessTier::Permissive,
    };
    zestroku_core::mutation::issue_approval(&store, &device, &command, now)
        .expect("mint approval token")
}

#[test]
fn http_launch_then_key_execute_once_with_audit() {
    let sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let target = format!("127.0.0.1:{}", sim.port());
    seed(&target, &db);
    let daemon_port = free_port();
    let _daemon = start(&db, daemon_port);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;

    // Unconfirmed launch: 403, audited, zero mutation POSTs (snapshot
    // GETs are permitted on local denial).
    let (status, body) = post(
        daemon_port,
        &format!("/v1/devices/{target}/launch"),
        r#"{"app_id": 1001}"#,
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("confirmation_required"), "{body}");
    let dispatch_posts = || {
        sim.received_requests()
            .into_iter()
            .filter(|(method, _)| method == "POST")
            .collect::<Vec<_>>()
    };
    assert!(
        dispatch_posts().is_empty(),
        "unconfirmed launch dispatched: {:?}",
        dispatch_posts()
    );
    // Approved launch executes exactly once with a matching audit row.
    let token = mint_token(&db, "127.0.0.1", sim.port(), "/launch/1001", now);
    let (status, body) = post(
        daemon_port,
        &format!("/v1/devices/{target}/launch"),
        &format!(r#"{{"app_id": 1001, "confirm": "{token}"}}"#),
    );
    assert_eq!(status, 200, "{body}");
    let outcome: serde_json::Value = serde_json::from_str(&body).expect("launch JSON");
    assert_eq!(outcome["outcome"], "completed", "{body}");
    let action_id = outcome["action_id"].as_i64().expect("action id");
    assert_eq!(outcome["surface"], "launch", "{body}");
    let store = SqliteStore::open(&db).expect("open store for audit");
    let row = store
        .get_action(action_id)
        .expect("read audit row")
        .expect("audit row exists");
    assert_eq!(row.decision, "approved", "{row:?}");
    assert_eq!(row.client, "http", "{row:?}");
    assert!(row.intent.contains("/launch/1001"), "{row:?}");
    assert!(row.before_state.contains("tier_source="), "{row:?}");
    let posts: Vec<_> = sim
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .collect();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(posts[0].1.ends_with("/launch/1001"), "{posts:?}");

    // Replay of the same token: 403, no second dispatch.
    let (status, _) = post(
        daemon_port,
        &format!("/v1/devices/{target}/launch"),
        &format!(r#"{{"app_id": 1001, "confirm": "{token}"}}"#),
    );
    assert_eq!(status, 403);
    assert_eq!(
        sim.received_requests()
            .into_iter()
            .filter(|(method, _)| method == "POST")
            .count(),
        1
    );

    // Keypress follows the same gate through the same daemon.
    let key_token = mint_token(&db, "127.0.0.1", sim.port(), "/keypress/Home", now);
    let (status, body) = post(
        daemon_port,
        &format!("/v1/devices/{target}/key"),
        &format!(r#"{{"key": "Home", "confirm": "{key_token}"}}"#),
    );
    assert_eq!(status, 200, "{body}");
    let outcome: serde_json::Value = serde_json::from_str(&body).expect("key JSON");
    assert_eq!(outcome["outcome"], "completed", "{body}");
    let posts: Vec<_> = sim
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .collect();
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert!(posts[1].1.ends_with("/keypress/Home"), "{posts:?}");
}
