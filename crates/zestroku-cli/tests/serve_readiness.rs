//! `zestroku serve` readiness and interrupt-shutdown contract.
#![forbid(unsafe_code)]

use std::io::{BufRead, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

fn wait_until_healthy(port: u16, host: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if probe_health_status(port, host) == Some(200) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "serve did not return /health 200 on {host} within 30s"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn bind_conflict_exits_without_readiness_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve bind");
    let port = listener.local_addr().expect("reserved address").port();
    let bind = format!("127.0.0.1:{port}");
    let output = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .args(["serve", "--bind", &bind])
        .output()
        .expect("run conflicting serve");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "serve unexpectedly succeeded with an occupied port: {output:?}"
    );
    assert!(
        !stderr.contains("listening on"),
        "failed bind claimed readiness: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn sigint_stops_serve_after_health_readiness_and_releases_listener() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let stderr_path = dir.path().join("serve.stderr");
    let stderr_file = std::fs::File::create(&stderr_path).expect("create stderr capture");
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let child = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .args(["serve", "--bind", &host])
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .expect("spawn zestroku serve");
    let mut daemon = Daemon(child);

    wait_until_healthy(port, &host);
    let ready_log = std::fs::read_to_string(&stderr_path).expect("read readiness log");
    let ready_line = format!("listening on http://{host}/");
    assert!(ready_log.contains(&ready_line), "{ready_log}");
    assert_eq!(probe_health_status(port, &host), Some(200));

    let pid = daemon.0.id().to_string();
    let signal = Command::new("kill")
        .args(["-INT", pid.as_str()])
        .status()
        .expect("send SIGINT to the child daemon");
    assert!(signal.success(), "SIGINT delivery failed: {signal}");
    let exit_deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(status) = daemon.0.try_wait().expect("wait for graceful exit") {
            break status;
        }
        assert!(
            Instant::now() < exit_deadline,
            "serve did not finish SIGINT shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let listener_released = TcpStream::connect(("127.0.0.1", port)).is_err();
    let stderr = std::fs::read_to_string(&stderr_path).expect("read final stderr");
    eprintln!(
        "m7c-sigint pid={pid} ready_health=200 exit={:?} listener_released={listener_released} stderr={stderr:?}",
        exit.code()
    );
    assert_eq!(exit.code(), Some(0), "SIGINT drain exit: {exit}");
    assert!(
        listener_released,
        "serve listener remains bound after SIGINT exit"
    );
}
