//! `zestroku serve`: the real binary listens, answers `/health` and a
//! store-backed `/v1/devices`, refuses a foreign Host header, and refuses to
//! start on a wildcard or public bind. No LAN: the store holds a seeded row.
#![forbid(unsafe_code)]

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use zestroku_core::store::{DeviceRow, SqliteStore, Store};

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr").port()
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
        .stderr(Stdio::inherit())
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

/// One HTTP/1.1 GET with an explicit Host; returns (status, body).
fn get(port: u16, path: &str, host: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .expect("write");
    let mut raw = String::new();
    let _ = s.read_to_string(&mut raw);
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
    (status, body)
}

fn read_response(reader: &mut std::io::BufReader<TcpStream>) -> (u16, String, String) {
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .expect("read HTTP status line");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    let mut content_length = None;
    let mut connection = String::new();
    loop {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .expect("read HTTP response header");
        assert_ne!(
            bytes, 0,
            "EOF before response header terminator: {status_line}"
        );
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok();
            } else if name.eq_ignore_ascii_case("connection") {
                connection = value.trim().to_string();
            }
        }
    }

    let content_length = content_length.expect("HTTP response Content-Length");
    let mut body = vec![0; content_length];
    reader
        .read_exact(&mut body)
        .expect("read HTTP response body");
    let body = String::from_utf8(body).expect("HTTP response body UTF-8");
    (status, connection, body)
}
#[test]
fn serve_answers_health_and_devices_and_checks_host() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    SqliteStore::open(&db)
        .expect("seed store")
        .upsert_device(&DeviceRow {
            id: "192.0.2.10:8060".into(),
            name: String::new(),
            ip: "192.0.2.10".into(),
            model: String::new(),
            model_num: "SIM-1234X".into(),
            firmware: "15.3.4".into(),
            wifi_driver: String::new(),
            access_tier: "permissive".into(),
            last_seen: 7,
        })
        .expect("seed device");

    let port = free_port();
    let _daemon = start(&db, port);
    let host = format!("127.0.0.1:{port}");

    let (status, body) = get(port, "/health", &host);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"ok\""), "{body}");

    let (status, body) = get(port, "/v1/devices", &host);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("SIM-1234X"), "{body}");
    assert!(body.contains("192.0.2.10"), "{body}");

    // Planted negative: a Host the listener was not bound for is refused.
    let (status, body) = get(port, "/health", "evil.example");
    assert!(
        (400..500).contains(&status),
        "foreign Host must be refused, got {status}: {body}"
    );
}

#[test]
fn serve_refuses_wildcard_and_public_binds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    for bind in ["0.0.0.0:8070", "8.8.8.8:8070"] {
        let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
            .env("ZESTROKU_DB", &db)
            .args(["serve", "--bind", bind])
            .output()
            .expect("run serve");
        assert_eq!(out.status.code(), Some(2), "{bind}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("--bind"), "{bind}: {err}");
    }
}
fn seed_sim_device(db: &std::path::Path, target: &str) {
    let host = target.rsplit_once(':').map_or(target, |(host, _)| host);
    SqliteStore::open(db)
        .expect("open sim inventory")
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
}

fn run_daemon_cli(db: &std::path::Path, daemon_url: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .env("ZESTROKU_DAEMON_URL", daemon_url)
        .env_remove("ZESTROKU_DIRECT")
        .args(args)
        .output()
        .expect("run zestroku daemon client")
}

#[test]
fn info_apps_and_active_app_use_the_running_daemon_for_simulator_reads() {
    let sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let target = format!("127.0.0.1:{}", sim.port());
    seed_sim_device(&db, &target);

    let daemon_port = free_port();
    let _daemon = start(&db, daemon_port);
    let daemon_url = format!("http://127.0.0.1:{daemon_port}");

    let info = run_daemon_cli(&db, &daemon_url, &["info", &target]);
    let stdout = String::from_utf8_lossy(&info.stdout);
    let stderr = String::from_utf8_lossy(&info.stderr);
    assert_eq!(
        info.status.code(),
        Some(0),
        "stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("tier=permissive"), "{stdout}");
    assert!(stdout.contains("tv-active-channel=absent"), "{stdout}");

    let active_app = run_daemon_cli(&db, &daemon_url, &["active-app", &target]);
    let stdout = String::from_utf8_lossy(&active_app.stdout);
    let stderr = String::from_utf8_lossy(&active_app.stderr);
    assert_eq!(
        active_app.status.code(),
        Some(0),
        "stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("Roku"), "{stdout}");

    let apps = run_daemon_cli(&db, &daemon_url, &["apps", &target, "--json"]);
    let stdout = String::from_utf8_lossy(&apps.stdout);
    let stderr = String::from_utf8_lossy(&apps.stderr);
    assert_eq!(
        apps.status.code(),
        Some(0),
        "stdout={stdout} stderr={stderr}"
    );
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("CLI apps JSON");
    let rows = document["apps"].as_array().expect("installed app rows");
    assert!(
        rows.iter()
            .any(|app| app["id"] == "1001" && app["name"] == "Streaming One"),
        "{document}"
    );
}

#[test]
fn missing_daemon_fails_fast_without_falling_back_to_the_live_simulator() {
    let sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let target = format!("127.0.0.1:{}", sim.port());
    seed_sim_device(&db, &target);

    let daemon_url = format!("http://127.0.0.1:{}", free_port());
    for verb in ["info", "apps"] {
        let started = Instant::now();
        let output = run_daemon_cli(&db, &daemon_url, &[verb, &target]);
        let elapsed = started.elapsed();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_ne!(output.status.code(), Some(0), "{verb}: {output:?}");
        assert!(stderr.contains("DAEMON_UNAVAILABLE"), "{verb}: {stderr}");
        assert!(
            elapsed < Duration::from_secs(2),
            "{verb}: elapsed={elapsed:?}"
        );
        assert!(
            sim.received_requests().is_empty(),
            "{verb} fell back to direct ECP while its daemon was unavailable"
        );
    }
}

fn probe_health_status(port: u16, host: &str) -> Option<u16> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut reader = std::io::BufReader::new(stream);
    let mut status_line = String::new();
    if reader.read_line(&mut status_line).ok()? == 0 {
        return None;
    }
    status_line.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(unix)]
#[test]
fn sigterm_stops_admission_and_drains_an_admitted_info_read() {
    let sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let target = format!("127.0.0.1:{}", sim.port());
    seed_sim_device(&db, &target);

    let port = free_port();
    let mut daemon = start(&db, port);
    let host = format!("127.0.0.1:{port}");
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set admitted-read timeout");
    let mut admitted = std::io::BufReader::new(stream);

    // Complete a health request on this same socket first. Its response proves
    // the server accepted the connection; keep-alive makes the next request
    // an admitted in-flight read on that connection.
    write!(
        admitted.get_mut(),
        "GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: keep-alive\r\n\r\n"
    )
    .expect("write same-socket health request");
    let (health_status, health_connection, health_body) = read_response(&mut admitted);
    assert_eq!(health_status, 200, "{health_body}");
    assert!(health_body.contains("\"ok\""), "{health_body}");
    assert!(
        !health_connection.eq_ignore_ascii_case("close"),
        "health connection closed before the follow-up request: {health_connection:?}"
    );

    write!(
        admitted.get_mut(),
        "GET /v1/devices/{target}/info HTTP/1.1\r\nHost: {host}\r\nContent-Length: 4\r\nConnection: close\r\n\r\nab"
    )
    .expect("send partial info request");

    assert!(
        sim.received_requests().is_empty(),
        "the incomplete HTTP request must not dispatch to the simulator"
    );

    let pid = daemon.0.id().to_string();
    let signal = Command::new("kill")
        .args(["-TERM", pid.as_str()])
        .status()
        .expect("send SIGTERM to the child daemon");
    assert!(signal.success(), "SIGTERM delivery failed: {signal}");

    // Poll the real health route until admission stops; a fixed delay would
    // only guess when the Asupersync signal and accept loop have converged.
    let stop_deadline = Instant::now() + Duration::from_secs(5);
    let mut stopped_probes = 0;
    loop {
        assert!(
            daemon.0.try_wait().expect("poll daemon").is_none(),
            "daemon exited before the admitted info read was completed"
        );
        match probe_health_status(port, &host) {
            Some(200) => stopped_probes = 0,
            _ => stopped_probes += 1,
        }
        if stopped_probes >= 2 {
            break;
        }
        assert!(
            Instant::now() < stop_deadline,
            "serve continued accepting health requests after SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // Complete the already-admitted request after admission has stopped. The
    // app-aware drain must finish its real ECP read before the child exits.
    admitted
        .get_mut()
        .write_all(b"cd")
        .expect("complete info request body");
    let source_sha =
        std::env::var("ZESTROKU_SOURCE_SHA").unwrap_or_else(|_| "unavailable".to_string());
    let mut raw = Vec::new();
    let read_result = admitted.read_to_end(&mut raw);
    let header_end = raw.windows(4).position(|window| window == b"\r\n\r\n");
    let response_head = header_end.map_or_else(String::new, |end| {
        String::from_utf8_lossy(&raw[..end]).into_owned()
    });
    let response_body_bytes = header_end.map_or(0, |end| raw.len() - end - 4);
    let response_content_length = response_head.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    let response_connection = response_head.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("connection")
            .then_some(value.trim())
    });
    let simulator_requests = sim.received_requests();
    let child_after_read = daemon
        .0
        .try_wait()
        .expect("inspect daemon after response read");
    let body_complete = response_content_length == Some(response_body_bytes);
    eprintln!(
        "m7c-admitted-frame pid={pid} source_sha={source_sha} request=GET /v1/devices/{target}/info HTTP/1.1; Content-Length: 4; Connection: close; body=abcd read={read_result:?} response_head={response_head:?} response_connection={response_connection:?} response_content_length={response_content_length:?} response_body_bytes={response_body_bytes} body_complete={body_complete} sim_requests={simulator_requests:?} child_after_read={child_after_read:?} response_bytes={raw:?}"
    );
    assert!(
        read_result.is_ok(),
        "admitted response did not reach EOF: {read_result:?}; head={response_head:?}; content_length={response_content_length:?}; body_bytes={response_body_bytes}"
    );
    assert_eq!(
        response_content_length,
        Some(response_body_bytes),
        "response body framing mismatch: {response_head:?}"
    );
    assert!(
        response_connection.is_some_and(|value| value.eq_ignore_ascii_case("close")),
        "client requested close but response did not close: {response_head:?}"
    );
    let response_text = String::from_utf8_lossy(&raw);
    let status = response_head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let body = response_text.split("\r\n\r\n").nth(1).unwrap_or_default();
    assert_eq!(status, 200, "{response_text}");
    assert!(body.contains("SIMX1"), "{body}");

    assert!(
        simulator_requests
            .iter()
            .any(|(method, path)| method == "GET" && path == "/query/device-info"),
        "the admitted info read must reach the loopback simulator: {:?}",
        simulator_requests
    );

    let exit_deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(status) = daemon.0.try_wait().expect("wait for graceful exit") {
            break status;
        }
        assert!(
            Instant::now() < exit_deadline,
            "serve did not finish draining"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(exit.code(), Some(0), "SIGTERM drain exit: {exit}");
    let listener_released = TcpStream::connect(("127.0.0.1", port)).is_err();
    assert!(
        listener_released,
        "serve listener remains bound after daemon exit"
    );
    eprintln!(
        "m7c-sigterm pid={pid} source_sha={source_sha} health={health_status}:{health_body} info={status}:{body} sim={simulator_requests:?} http_connection={response_connection:?} content_length={response_content_length:?} body_bytes={response_body_bytes} body_complete={body_complete} exit={:?} listener_released={listener_released}",
        exit.code()
    );
}
