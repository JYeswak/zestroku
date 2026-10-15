#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use zestroku_core::store::{DeviceRow, SqliteStore, Store};
use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

struct Daemon(Child);

impl Daemon {
    fn stop(&mut self) -> (ExitStatus, String) {
        if self.0.try_wait().expect("poll serve child").is_none() {
            let pid = self.0.id().to_string();
            let signal = Command::new("kill")
                .args(["-TERM", pid.as_str()])
                .status()
                .expect("send SIGTERM to owned serve child");
            assert!(signal.success(), "SIGTERM delivery failed: {signal}");
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = self.0.try_wait().expect("poll graceful shutdown") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "serve did not stop after SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stderr = String::new();
        if let Some(mut pipe) = self.0.stderr.take() {
            pipe.read_to_string(&mut stderr)
                .expect("read serve diagnostics");
        }
        (status, stderr)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback port");
    listener.local_addr().expect("read port").port()
}

fn write_targets(path: &std::path::Path, ports: &[u16]) {
    let contents = ports
        .iter()
        .map(|port| format!("[[target]]\nhost = \"127.0.0.1\"\nport = {port}\n"))
        .collect::<String>();
    std::fs::write(path, contents).expect("write isolated targets.toml");
}

fn http_get(port: u16, path: &str) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or_default();
    let body = response
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    Ok((status, body))
}

fn read_inventory(port: u16) -> Vec<serde_json::Value> {
    let (status, body) = http_get(port, "/v1/devices").expect("fetch startup inventory");
    assert_eq!(status, 200, "inventory response={body}");
    serde_json::from_str::<serde_json::Value>(&body)
        .expect("inventory JSON")
        .as_array()
        .expect("inventory DTO is an array")
        .clone()
}

fn start_daemon(
    db: &std::path::Path,
    home: &std::path::Path,
    targets: &std::path::Path,
    port: u16,
) -> Daemon {
    let child = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .env("HOME", home)
        .env_remove("ZESTROKU_TARGETS")
        .args([
            "serve",
            "--bind",
            &format!("127.0.0.1:{port}"),
            "--no-ssdp",
            "--targets",
        ])
        .arg(targets)
        .args(["--mx", "1"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn real zestroku serve binary");
    let mut daemon = Daemon(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = daemon.0.try_wait().expect("poll serve child") {
            let mut stderr = String::new();
            if let Some(mut pipe) = daemon.0.stderr.take() {
                pipe.read_to_string(&mut stderr)
                    .expect("read startup diagnostics");
            }
            panic!("serve exited before health readiness: {status}; stderr={stderr}");
        }
        if let Ok((200, body)) = http_get(port, "/health") {
            assert!(body.contains("\"ok\""), "health body={body}");
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "serve did not become ready on {port}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fixture_identity(serial: &str, model: &str, model_number: &str, is_tv: bool) -> String {
    let xml = format!(
        "<device-info><udn>FIXTUREUDN</udn><serial-number>{serial}</serial-number><model-name>{model}</model-name><model-number>{model_number}</model-number><software-version>15.2.4</software-version><is-tv>{is_tv}</is-tv></device-info>"
    );
    let info =
        zestroku_proto::parse_device_info(&xml).expect("parse scrubbed SimDevice identity fixture");
    zestroku_core::discovery::identity_key(&info)
}

fn assert_persisted_identity_and_tier(
    db: &std::path::Path,
    port: u16,
    serial: &str,
    model: &str,
    model_number: &str,
    is_tv: bool,
    expected_tier: &str,
) -> i64 {
    let id = format!("127.0.0.1:{port}");
    let store = SqliteStore::open(db).expect("open startup store");
    let row = store
        .get_device(&id)
        .expect("read device row")
        .expect("device row persisted");
    assert_eq!(row.id, id);
    assert_eq!(row.access_tier, expected_tier);
    let tier = store
        .get_access_tier(&id)
        .expect("read recorded tier")
        .expect("startup must record actual query/apps observation");
    assert_eq!(tier.tier, expected_tier);
    let identity = fixture_identity(serial, model, model_number, is_tv);
    assert!(!identity.contains(serial) && !identity.contains("FIXTUREUDN"));
    assert_eq!(
        store
            .find_device_id_by_identity(&identity)
            .expect("identity lookup"),
        Some(id)
    );
    row.last_seen
}

fn persisted_row(db: &std::path::Path, port: u16) -> DeviceRow {
    SqliteStore::open(db)
        .expect("open last-known store")
        .get_device(&format!("127.0.0.1:{port}"))
        .expect("read last-known row")
        .expect("last-known device remains persisted")
}

#[test]
fn serve_startup_discovers_and_records_identity_and_limited_tier_before_info() {
    const SERIAL: &str = "FIXTURESERIAL-SERVE-STARTUP";
    let sim = SimDevice::start_with_serial(
        SimConfig {
            kind: DeviceKind::Express,
            tier: SimTier::Limited,
        },
        SERIAL,
    );
    let directory = tempfile::tempdir().expect("isolated startup directory");
    let db = directory.path().join("startup.db");
    let targets = directory.path().join("targets.toml");
    write_targets(&targets, &[sim.port()]);
    let port = free_port();
    let mut daemon = start_daemon(&db, directory.path(), &targets, port);
    let startup_requests = sim.received_requests();
    eprintln!("startup simulator requests={startup_requests:?}");

    let (status, inventory_body) =
        http_get(port, "/v1/devices").expect("fetch startup inventory before /info");
    assert_eq!(status, 200, "inventory response={inventory_body}");
    let inventory: serde_json::Value =
        serde_json::from_str(&inventory_body).expect("inventory JSON");
    let rows = inventory.as_array().expect("inventory DTO is an array");
    assert_eq!(
        rows.len(),
        1,
        "startup should persist one discovered row: {inventory}"
    );
    assert_eq!(rows[0]["host"], "127.0.0.1");
    assert_eq!(rows[0]["model"], "Sim Express");
    assert_eq!(rows[0]["model_number"], "SIMX1");
    assert_eq!(rows[0]["access_tier"], "limited");
    let target_id = format!("127.0.0.1:{}", sim.port());
    let identity = fixture_identity(SERIAL, "Sim Express", "SIMX1", false);
    assert_persisted_identity_and_tier(
        &db,
        sim.port(),
        SERIAL,
        "Sim Express",
        "SIMX1",
        false,
        "limited",
    );
    assert!(
        !inventory_body.contains(SERIAL)
            && !inventory_body.contains("FIXTUREUDN")
            && !inventory_body.contains("FIXTUREDEVICEID"),
        "inventory response leaked a fixture identifier: {inventory_body}"
    );
    assert_eq!(
        startup_requests,
        vec![
            ("GET".into(), "/query/device-info".into()),
            ("GET".into(), "/query/apps".into()),
        ],
        "startup must probe identity once and record the existing Limited 403 classifier result"
    );

    {
        let store = SqliteStore::open(&db).expect("open startup store for controlled omission");
        store
            .delete_access_tier(&target_id)
            .expect("remove startup tier observation");
        store
            .delete_device_identity(&target_id)
            .expect("remove startup identity record");
        store
            .delete_device(&target_id)
            .expect("remove startup inventory row");
        assert!(
            store
                .get_device(&target_id)
                .expect("verify row omission")
                .is_none()
        );
        assert!(
            store
                .find_device_id_by_identity(&identity)
                .expect("verify identity omission")
                .is_none()
        );
        assert!(
            store
                .get_access_tier(&target_id)
                .expect("verify tier omission")
                .is_none()
        );
    }
    let requests_before_fallback_probe = sim.received_requests();
    assert!(
        read_inventory(port).is_empty(),
        "API must not recreate an omitted startup row"
    );
    let (missing_status, missing_body) = http_get(port, &format!("/v1/devices/{target_id}/info"))
        .expect("request info for omitted startup row");
    assert_eq!(
        missing_status, 404,
        "missing row must not be discovered by API fallback: {missing_body}"
    );
    assert_eq!(
        sim.received_requests(),
        requests_before_fallback_probe,
        "API reads for an omitted row must not use a direct-device fallback"
    );
    let (exit, _) = daemon.stop();
    assert_eq!(exit.code(), Some(0), "serve exit after SIGTERM");
}

#[test]
fn duplicate_seed_and_restart_keep_one_canonical_identity_row() {
    const SERIAL: &str = "FIXTURESERIAL-SERVE-RESTART";
    let sim = SimDevice::start_with_serial(
        SimConfig {
            kind: DeviceKind::Express,
            tier: SimTier::Permissive,
        },
        SERIAL,
    );
    let directory = tempfile::tempdir().expect("isolated restart directory");
    let db = directory.path().join("restart.db");
    let targets = directory.path().join("targets.toml");
    write_targets(&targets, &[sim.port(), sim.port()]);

    let first_port = free_port();
    let mut first = start_daemon(&db, directory.path(), &targets, first_port);
    let rows = read_inventory(first_port);
    assert_eq!(
        rows.len(),
        1,
        "duplicate seed must still produce one canonical API row: {rows:?}"
    );
    assert_eq!(rows[0]["model_number"], "SIMX1");
    assert_eq!(rows[0]["access_tier"], "permissive");
    let first_last_seen = (
        assert_persisted_identity_and_tier(
            &db,
            sim.port(),
            SERIAL,
            "Sim Express",
            "SIMX1",
            false,
            "permissive",
        ),
        sim.received_requests(),
    );
    let (exit, _) = first.stop();
    assert_eq!(exit.code(), Some(0), "first serve shutdown");

    let second_port = free_port();
    let mut second = start_daemon(&db, directory.path(), &targets, second_port);
    let restart_requests = sim.received_requests();
    for path in ["/query/device-info", "/query/apps"] {
        let before = first_last_seen
            .1
            .iter()
            .filter(|(_, uri)| uri == path)
            .count();
        let after = restart_requests
            .iter()
            .filter(|(_, uri)| uri == path)
            .count();
        assert!(
            after > before,
            "restart must perform a fresh {path} probe; before={before} after={after} requests={restart_requests:?}"
        );
    }
    let rows = read_inventory(second_port);
    assert_eq!(
        rows.len(),
        1,
        "restart must not duplicate the canonical device row: {rows:?}"
    );
    assert_eq!(rows[0]["host"], "127.0.0.1");
    let second_last_seen = assert_persisted_identity_and_tier(
        &db,
        sim.port(),
        SERIAL,
        "Sim Express",
        "SIMX1",
        false,
        "permissive",
    );
    assert!(
        second_last_seen >= first_last_seen.0,
        "successful restart discovery should not move last_seen backwards"
    );
    let (exit, _) = second.stop();
    assert_eq!(exit.code(), Some(0), "second serve shutdown");
}

#[test]
fn suspended_startup_preserves_last_known_row_and_leaves_fresh_store_empty() {
    const SERIAL: &str = "FIXTURESERIAL-SERVE-SUSPEND";
    let mut sim = SimDevice::start_with_serial(
        SimConfig {
            kind: DeviceKind::Express,
            tier: SimTier::Permissive,
        },
        SERIAL,
    );
    let directory = tempfile::tempdir().expect("isolated suspend directory");
    let db = directory.path().join("known.db");
    let targets = directory.path().join("targets.toml");
    write_targets(&targets, &[sim.port()]);

    let first_port = free_port();
    let mut first = start_daemon(&db, directory.path(), &targets, first_port);
    let last_seen = assert_persisted_identity_and_tier(
        &db,
        sim.port(),
        SERIAL,
        "Sim Express",
        "SIMX1",
        false,
        "permissive",
    );
    let (exit, _) = first.stop();
    assert_eq!(exit.code(), Some(0), "initial serve shutdown");

    sim.suspend();
    let second_port = free_port();
    let mut second = start_daemon(&db, directory.path(), &targets, second_port);
    let known_rows = read_inventory(second_port);
    assert_eq!(
        known_rows.len(),
        1,
        "unreachable startup must retain last-known inventory"
    );
    assert_eq!(known_rows[0]["access_tier"], "permissive");
    let unchanged = persisted_row(&db, sim.port());
    assert_eq!(
        unchanged.last_seen, last_seen,
        "failed discovery must not refresh last_seen"
    );
    assert_eq!(
        unchanged.access_tier, "permissive",
        "failed discovery must preserve last-known tier"
    );
    let (exit, stderr) = second.stop();
    assert_eq!(
        exit.code(),
        Some(0),
        "serve should continue with retained inventory"
    );
    assert!(
        stderr.contains("startup discovery"),
        "missing failed-target note: {stderr}"
    );

    let empty_db = directory.path().join("empty.db");
    let third_port = free_port();
    let mut third = start_daemon(&empty_db, directory.path(), &targets, third_port);
    assert!(
        read_inventory(third_port).is_empty(),
        "suspended seed must not fabricate a device row"
    );
    let store = SqliteStore::open(&empty_db).expect("open empty startup store");
    assert!(
        store
            .list_devices()
            .expect("read empty inventory")
            .is_empty()
    );
    let (exit, stderr) = third.stop();
    assert_eq!(exit.code(), Some(0), "empty but healthy daemon shutdown");
    assert!(
        stderr.contains("startup discovery"),
        "missing empty-store failed-target note: {stderr}"
    );
}
