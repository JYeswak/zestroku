#![cfg(unix)]
#![forbid(unsafe_code)]

use std::io::{BufRead, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
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
    worker: Option<JoinHandle<()>>,
}
impl Proxy {
    fn start(backend: u16, hold_path: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (arrived_tx, arrived) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut held = false;
            for client in listener.incoming() {
                let Ok(mut client) = client else { break };
                let (bytes, header) = match read_req(&mut client) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let path = header.split_whitespace().nth(1).unwrap_or("");
                if path.contains(hold_path) && !held {
                    held = true;
                    let _ = arrived_tx.send(());
                    let _ = release_rx.recv();
                }
                let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", backend)) else {
                    continue;
                };
                let _ = upstream.write_all(&bytes);
                let _ = client.set_read_timeout(Some(Duration::from_secs(20)));
                let _ = upstream.set_read_timeout(Some(Duration::from_secs(20)));
                let _ = std::io::copy(&mut upstream, &mut client);
                let _ = client.shutdown(Shutdown::Both);
            }
        });
        Self {
            port,
            arrived,
            release,
            worker: Some(worker),
        }
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
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
    let proxy = Proxy::start(sim.port(), "/query/tv-active-channel");
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
    let proxy = Proxy::start(sim.port(), "/query/tv-active-channel");
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
    let pid = daemon.0.id().unwrap().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if probe_no_health(port, &host) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    proxy.release.send(()).unwrap();
    let (status, response) = request_thread.join().unwrap();
    eprintln!(
        "C2 status={status} response={response:?} sim={:?}",
        sim.received_requests()
    );
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("SIMX1"), "{response}");
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

fn probe_no_health(port: u16, host: &str) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return true;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let _ = write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    );
    let mut byte = [0; 1];
    stream.read(&mut byte).is_err()
}
