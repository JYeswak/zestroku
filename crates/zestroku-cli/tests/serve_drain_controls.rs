#![cfg(unix)]
#![forbid(unsafe_code)]

use std::io::{BufRead, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use zestroku_core::store::{DeviceRow, SqliteStore, Store};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start(db: &std::path::Path, port: u16) -> Daemon {
    let child = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .args(["serve", "--bind", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn daemon");
    let daemon = Daemon(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return daemon;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon failed to listen");
}

fn seed(db: &std::path::Path, target: &str) {
    SqliteStore::open(db)
        .unwrap()
        .upsert_device(&DeviceRow {
            id: target.into(),
            name: "Sim Roku".into(),
            ip: "127.0.0.1".into(),
            model: "Roku Express".into(),
            model_num: "SIM-1234X".into(),
            firmware: "15.2.4".into(),
            wifi_driver: "fixture".into(),
            access_tier: "permissive".into(),
            last_seen: 1,
        })
        .unwrap();
}

fn request(port: u16, host: &str, target: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(40)))
        .unwrap();
    write!(
        stream,
        "GET /v1/devices/{target}/info HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, raw)
}

fn partial_info_request(port: u16, host: &str, target: &str) -> (TcpStream, Instant) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(40)))
        .unwrap();
    let started = Instant::now();
    write!(
        stream,
        "GET /v1/devices/{target}/info HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: 4\r\n\r\nab"
    )
    .unwrap();
    (stream, started)
}

fn status_code(response: &[u8]) -> Option<u16> {
    std::str::from_utf8(response)
        .ok()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn read_req(stream: &mut TcpStream) -> std::io::Result<(Vec<u8>, String)> {
    let mut reader = std::io::BufReader::new(stream.try_clone()?);
    let mut request = Vec::new();
    let mut line = String::new();
    let mut content_length = 0;
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok((request, String::new()));
        }
        request.extend_from_slice(line.as_bytes());
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        if line == "\r\n" {
            break;
        }
    }
    let header = String::from_utf8_lossy(&request).into_owned();
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    request.extend_from_slice(&body);
    Ok((request, header))
}

struct Proxy {
    port: u16,
    arrived: Receiver<()>,
    release: Sender<()>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Proxy {
    fn start(backend: u16, hold_path: Option<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (arrived_tx, arrived) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let mut held = false;
            for client in listener.incoming() {
                let Ok(mut client) = client else { break };
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                let (bytes, header) = match read_req(&mut client) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let path = header.split_whitespace().nth(1).unwrap_or("");
                if hold_path.is_some_and(|needle| path.contains(needle)) && !held {
                    held = true;
                    let _ = arrived_tx.send(());
                    let _ = release_rx.recv_timeout(Duration::from_secs(20));
                }
                let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", backend)) else {
                    continue;
                };
                if upstream.write_all(&bytes).is_err() {
                    continue;
                }
                let mut response = std::io::BufReader::new(upstream);
                let mut head = Vec::new();
                let mut line = Vec::new();
                let mut length = 0usize;
                loop {
                    line.clear();
                    match response.read_until(b'\n', &mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if let Some(colon) = line.iter().position(|b| *b == b':') {
                        if line[..colon].eq_ignore_ascii_case(b"content-length") {
                            length = std::str::from_utf8(&line[colon + 1..])
                                .ok()
                                .and_then(|v| v.trim().parse().ok())
                                .unwrap_or(0);
                        }
                    }
                    let done = line == b"\r\n";
                    head.extend_from_slice(&line);
                    if done {
                        break;
                    }
                }
                if client.write_all(&head).is_err() {
                    continue;
                }
                let mut body = vec![0; length];
                if response.read_exact(&mut body).is_err() {
                    continue;
                }
                let _ = client.write_all(&body);
                let _ = client.shutdown(Shutdown::Both);
            }
        });
        Self {
            port,
            arrived,
            release,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.release.send(());
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn ecp_sim() -> zestroku_sim::SimDevice {
    zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    })
}

#[test]
fn c1_no_term_info_baseline_returns_200() {
    let sim = ecp_sim();
    let proxy = Proxy::start(sim.port(), None);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    let target = format!("127.0.0.1:{}", proxy.port);
    seed(&db, &target);
    let port = free_port();
    let _daemon = start(&db, port);
    let host = format!("127.0.0.1:{port}");
    let (status, response) = request(port, &host, &target);
    eprintln!(
        "C1 status={status} response={response:?} sim={:?}",
        sim.received_requests()
    );
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("SIMX1"), "{response}");
}

#[test]
fn c2_inflight_proxy_held_info_completes_after_term() {
    let sim = ecp_sim();
    let proxy = Proxy::start(sim.port(), Some("/query/tv-active-channel"));
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    let target = format!("127.0.0.1:{}", proxy.port);
    seed(&db, &target);
    let port = free_port();
    let mut daemon = start(&db, port);
    let host = format!("127.0.0.1:{port}");
    let request_thread = thread::spawn({
        let host = host.clone();
        let target = target.clone();
        move || request(port, &host, &target)
    });
    proxy
        .arrived
        .recv_timeout(Duration::from_secs(10))
        .expect("proxy did not observe held tv-active-channel request");
    let held_at = Instant::now();
    let pid = daemon.0.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let term_at = Instant::now();
    let stopped_after = wait_for_admission_stopped(&mut daemon, port, &host, term_at, "C2");
    let held_for = held_at.elapsed();
    assert!(
        held_for < Duration::from_millis(2500),
        "proxy hold exceeded half the 5s ECP transport timeout: {held_for:?}"
    );
    eprintln!("C2 admission_stopped_after={stopped_after:?} held_for={held_for:?}");
    proxy.release.send(()).unwrap();
    let (status, response) = request_thread.join().unwrap();
    eprintln!(
        "C2 held_for={held_for:?} status={status} response={response:?} sim={:?}",
        sim.received_requests()
    );
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("SIMX1"), "{response}");
    assert!(
        response.contains("\"tv_active_channel\":false"),
        "held ECP result did not reach the HTTP response: {response}"
    );
    let exit_deadline = Instant::now() + Duration::from_secs(40);
    let exit = loop {
        if let Some(s) = daemon.0.try_wait().unwrap() {
            break s;
        }
        assert!(
            Instant::now() < exit_deadline,
            "daemon did not exit after drain timeout"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(exit.code(), Some(0), "{exit}");
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "listener still open"
    );
}

#[test]
fn c4_partial_info_body_completes_without_term() {
    let sim = ecp_sim();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    let target = format!("127.0.0.1:{}", sim.port());
    seed(&db, &target);
    let port = free_port();
    let _daemon = start(&db, port);
    let host = format!("127.0.0.1:{port}");
    let (mut stream, started) = partial_info_request(port, &host, &target);
    eprintln!(
        "C4 frame=sent_headers_plus_ab elapsed={:?}",
        started.elapsed()
    );
    thread::sleep(Duration::from_millis(250));
    stream.write_all(b"cd").unwrap();
    eprintln!("C4 frame=sent_cd elapsed={:?}", started.elapsed());
    let mut response = Vec::new();
    let read = stream.read_to_end(&mut response);
    let elapsed_to_eof = started.elapsed();
    let text = String::from_utf8_lossy(&response);
    eprintln!(
        "C4 read={read:?} elapsed_to_eof={elapsed_to_eof:?} status={:?} response={text:?} sim={:?}",
        status_code(&response),
        sim.received_requests()
    );
    assert!(
        read.is_ok(),
        "complete partial-body baseline failed: {read:?}"
    );
    assert_eq!(status_code(&response), Some(200), "{text}");
    assert!(text.contains("SIMX1"), "{text}");
}

#[test]
fn c3_partial_info_body_completes_after_term_and_admission_stop() {
    let sim = ecp_sim();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    let target = format!("127.0.0.1:{}", sim.port());
    seed(&db, &target);
    let port = free_port();
    let mut daemon = start(&db, port);
    let host = format!("127.0.0.1:{port}");
    let (mut stream, started) = partial_info_request(port, &host, &target);
    eprintln!(
        "C3 source={} frame=sent_headers_plus_ab elapsed={:?}",
        std::env::var("RCH_BUILD_SOURCE").unwrap_or_else(|_| "unavailable".into()),
        started.elapsed()
    );
    thread::sleep(Duration::from_millis(250));
    let pid = daemon.0.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success(),
        "failed to deliver SIGTERM"
    );
    let term_at = Instant::now();
    let stopped_after = wait_for_admission_stopped(&mut daemon, port, &host, term_at, "C3");
    eprintln!("C3 frame=admission_stopped elapsed_since_term={stopped_after:?}");
    let send_cd = stream.write_all(b"cd");
    eprintln!(
        "C3 frame=sent_cd result={send_cd:?} elapsed_since_term={:?}",
        term_at.elapsed()
    );
    let mut response = Vec::new();
    let read = stream.read_to_end(&mut response);
    let elapsed_to_eof = started.elapsed();
    let elapsed_since_term = term_at.elapsed();
    let text = String::from_utf8_lossy(&response);
    eprintln!(
        "C3 read={read:?} elapsed_to_eof={elapsed_to_eof:?} elapsed_since_term={elapsed_since_term:?} status={:?} response={text:?} sim={:?}",
        status_code(&response),
        sim.received_requests()
    );
    let exit_deadline = term_at + Duration::from_secs(40);
    while daemon.0.try_wait().unwrap().is_none() && Instant::now() < exit_deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let exit = daemon.0.try_wait().unwrap();
    eprintln!(
        "C3 daemon_exit={:?} elapsed_since_term={:?}",
        exit.map(|status| status.code()),
        term_at.elapsed()
    );
}

#[derive(Debug, Clone, Copy)]
enum HealthProbe {
    Refused,
    Reset,
    Eof,
    Timeout,
    Status(u16),
}

fn classify_probe_error(error: std::io::Error) -> HealthProbe {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => HealthProbe::Refused,
        std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::BrokenPipe => HealthProbe::Reset,
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => HealthProbe::Timeout,
        _ => panic!("unexpected health probe I/O error: {error}"),
    }
}

fn probe_health_status(port: u16, host: &str) -> HealthProbe {
    let stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(stream) => stream,
        Err(error) => return classify_probe_error(error),
    };
    if let Err(error) = stream.set_read_timeout(Some(Duration::from_millis(200))) {
        return classify_probe_error(error);
    }
    let mut stream = stream;
    if let Err(error) = write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    ) {
        return classify_probe_error(error);
    }
    let mut reader = std::io::BufReader::new(stream);
    let mut status_line = String::new();
    match reader.read_line(&mut status_line) {
        Ok(0) => HealthProbe::Eof,
        Ok(_) => HealthProbe::Status(
            status_line
                .split_whitespace()
                .nth(1)
                .and_then(|status| status.parse().ok())
                .unwrap_or(0),
        ),
        Err(error) => classify_probe_error(error),
    }
}

fn wait_for_admission_stopped(
    daemon: &mut Daemon,
    port: u16,
    host: &str,
    term_at: Instant,
    label: &str,
) -> Duration {
    let stop_deadline = Instant::now() + Duration::from_secs(5);
    let mut stopped_probes = 0;
    let mut probes = 0;
    loop {
        assert!(
            daemon.0.try_wait().expect("poll daemon").is_none(),
            "{label}: daemon exited before the admitted request completed"
        );
        let result = probe_health_status(port, host);
        probes += 1;
        if probes <= 5 {
            eprintln!(
                "{label} health_probe={probes} elapsed_since_term={:?} classification={result:?}",
                term_at.elapsed()
            );
        }
        if matches!(result, HealthProbe::Status(200)) {
            stopped_probes = 0;
        } else {
            stopped_probes += 1;
        }
        if stopped_probes >= 2 {
            return term_at.elapsed();
        }
        assert!(
            Instant::now() < stop_deadline,
            "{label}: health probe still returns status 200 after SIGTERM"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
