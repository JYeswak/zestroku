//! Slice-1 acceptance for zestroku-sim, over real loopback sockets with a raw
//! std TcpStream client (so response bytes are asserted exactly).
#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use zestroku_proto::{
    EcpGate, EcpResponse, parse_active_app, parse_device_info, parse_ecp_response,
};
use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

struct Raw {
    status: u16,
    head: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Raw {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn ecp(&self) -> EcpResponse {
        EcpResponse {
            status: self.status,
            headers: self.headers.clone(),
            body: self.body.clone(),
        }
    }
}

fn request(port: u16, method: &str, path: &str) -> Raw {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect sim");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("write timeout");
    let wire = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nAccept: */*\r\n\r\n"
    );
    stream.write_all(wire.as_bytes()).expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let text = String::from_utf8(raw).expect("utf-8 response");
    let (head, body) = text.split_once("\r\n\r\n").expect("header/body split");
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status line");
    let headers = lines
        .filter_map(|line| line.split_once(": "))
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
    Raw {
        status,
        head: format!("{head}\r\n\r\n"),
        headers,
        body: body.to_string(),
    }
}

fn get(dev: &SimDevice, path: &str) -> Raw {
    request(dev.port(), "GET", path)
}

fn post(dev: &SimDevice, path: &str) -> Raw {
    request(dev.port(), "POST", path)
}

fn sim(kind: DeviceKind, tier: SimTier) -> SimDevice {
    SimDevice::start(SimConfig { kind, tier })
}

const LIMITED_HEAD: &str = "HTTP/1.1 403 Forbidden\r\n\
Server: Roku/15.2.4 UPnP/1.0 Roku/15.2.4\r\n\
Content-Length: 40\r\n\
Cache-Control: no-cache\r\n\
Content-Type: text/plain; charset=\"utf-8\"\r\n";

// ---- 1. Limited 403 bytes ------------------------------------------------

#[test]
fn limited_403_is_byte_exact() {
    let dev = sim(DeviceKind::Express, SimTier::Limited);
    for (method, path) in [
        ("GET", "/query/apps"),
        ("GET", "/query/media-player"),
        ("POST", "/keypress/Home"),
    ] {
        let r = request(dev.port(), method, path);
        assert_eq!(r.status, 403, "{method} {path}");
        assert_eq!(
            r.body, "ECP command not allowed in Limited mode.",
            "{method} {path}"
        );
        assert_eq!(r.body.len(), 40);
        assert_eq!(r.header("Content-Length"), Some("40"));
        assert_eq!(
            r.header("Content-Type"),
            Some("text/plain; charset=\"utf-8\"")
        );
        assert_eq!(r.header("Cache-Control"), Some("no-cache"));
        assert_eq!(r.header("Server"), Some("Roku/15.2.4 UPnP/1.0 Roku/15.2.4"));
        // The captured fixture (ecp_fixture_limited_response.txt) pins header
        // ORDER: Server, Content-Length, Cache-Control, Content-Type. The
        // h1 server additionally appends `Connection: close` because THIS
        // test client requests it — transport negotiation, not device
        // behavior — so it is stripped before the order comparison, along
        // with the blank-line separator the split leaves behind.
        let normalized = r
            .head
            .replace("Connection: close\r\n", "")
            .trim_end_matches("\r\n")
            .to_string()
            + "\r\n";
        assert_eq!(normalized, LIMITED_HEAD, "{method} {path}");
    }
}

// ---- 2. wrapper-not-filter property ---------------------------------------

struct Row {
    route: String,
    limited: u16,
    permissive: u16,
}

/// The invariant: a Limited 403 may only become 200 or 404 under Permissive.
fn assert_wrapper_not_filter(rows: &[Row]) {
    for row in rows {
        if row.limited == 403 {
            assert!(
                row.permissive == 200 || row.permissive == 404,
                "wrapper-not-filter violated: {} is 403 under Limited but {} under Permissive",
                row.route,
                row.permissive
            );
        }
    }
}

fn table() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GET", "/query/device-info"),
        ("GET", "/query/active-app"),
        ("GET", "/query/apps"),
        ("GET", "/query/icon/1001"),
        ("GET", "/query/icon/9999"),
        ("GET", "/query/media-player"),
        ("GET", "/query/tv-active-channel"),
        ("GET", "/query/rid"),
        ("GET", "/query/dev-mode"),
        ("GET", "/keypress/Home"),
        ("GET", "/launch/1001"),
        ("POST", "/keypress/Home"),
        ("POST", "/launch/1001"),
        ("POST", "/launch/9999"),
    ]
}

fn walk(kind: DeviceKind) -> Vec<Row> {
    let dev = sim(kind, SimTier::Limited);
    let limited: Vec<u16> = table()
        .iter()
        .map(|(m, p)| request(dev.port(), m, p).status)
        .collect();
    dev.set_tier(SimTier::Permissive);
    let permissive: Vec<u16> = table()
        .iter()
        .map(|(m, p)| request(dev.port(), m, p).status)
        .collect();
    table()
        .iter()
        .zip(limited.into_iter().zip(permissive))
        .map(|((m, p), (limited, permissive))| Row {
            route: format!("{m} {p}"),
            limited,
            permissive,
        })
        .collect()
}

#[test]
fn wrapper_not_filter_over_the_whole_table() {
    for kind in [DeviceKind::Express, DeviceKind::Tv] {
        let rows = walk(kind);
        assert_wrapper_not_filter(&rows);
        // Non-vacuous: the gate really closes some routes under Limited, and
        // every one of them opens (200) or disappears (404) under Permissive.
        let gated: Vec<_> = rows.iter().filter(|r| r.limited == 403).collect();
        assert!(
            gated.len() >= 5,
            "{kind:?}: only {} gated rows",
            gated.len()
        );
        assert!(
            rows.iter().all(|r| r.permissive != 403),
            "{kind:?}: a Permissive 403 exists"
        );
    }
}

/// Planted negative: a router that still answers 403 under Permissive (a
/// filter instead of a wrapper) MUST fail the property. `should_panic` makes
/// this test red if `assert_wrapper_not_filter` ever stops catching it.
#[test]
#[should_panic(expected = "wrapper-not-filter violated")]
fn planted_negative_permissive_403_is_caught() {
    let rows = [
        Row {
            route: "GET /query/apps".into(),
            limited: 403,
            permissive: 200,
        },
        Row {
            route: "GET /query/media-player".into(),
            limited: 403,
            permissive: 403,
        },
    ];
    assert_wrapper_not_filter(&rows);
}

// ---- 3. tier flip -----------------------------------------------------------

#[test]
fn tier_flip_opens_apps_on_the_same_server() {
    let dev = sim(DeviceKind::Express, SimTier::Limited);
    let port = dev.port();
    assert_eq!(get(&dev, "/query/apps").status, 403);
    dev.set_tier(SimTier::Permissive);
    assert_eq!(dev.port(), port);
    let r = get(&dev, "/query/apps");
    assert_eq!(r.status, 200);
    assert!(
        r.body
            .contains("<app id=\"1001\" subtype=\"svod\" type=\"appl\">Streaming One</app>")
    );
    dev.set_tier(SimTier::Limited);
    assert_eq!(get(&dev, "/query/apps").status, 403);
}

// ---- 4. TV-only surface -----------------------------------------------------

#[test]
fn tv_active_channel_is_tv_only() {
    let tv = sim(DeviceKind::Tv, SimTier::Limited);
    assert_eq!(get(&tv, "/query/tv-active-channel").status, 403);
    tv.set_tier(SimTier::Permissive);
    assert_eq!(get(&tv, "/query/tv-active-channel").status, 200);

    let express = sim(DeviceKind::Express, SimTier::Limited);
    assert_eq!(get(&express, "/query/tv-active-channel").status, 404);
    express.set_tier(SimTier::Permissive);
    assert_eq!(get(&express, "/query/tv-active-channel").status, 404);
}

// ---- 5. mutation --------------------------------------------------------------

fn active(dev: &SimDevice) -> zestroku_proto::ActiveApp {
    let r = get(dev, "/query/active-app");
    assert_eq!(r.status, 200);
    parse_active_app(&r.body).expect("parse active-app")
}

#[test]
fn permissive_launch_mutates_and_get_never_does() {
    let dev = sim(DeviceKind::Express, SimTier::Permissive);
    assert_eq!(active(&dev).app_id, None);

    assert_eq!(post(&dev, "/launch/1001").status, 200);
    let now = active(&dev);
    assert_eq!(now.app_id, Some(1001));
    assert_eq!(now.name, "Streaming One");
    assert_eq!(dev.launch_count(), 1);

    // GET never works, and must not mutate.
    assert_eq!(get(&dev, "/launch/1002").status, 404);
    assert_eq!(active(&dev).app_id, Some(1001));

    // Absent id: 404, state unchanged.
    assert_eq!(post(&dev, "/launch/9999").status, 404);
    assert_eq!(active(&dev).app_id, Some(1001));
    assert_eq!(dev.launch_count(), 1);

    // Home resets the active app.
    assert_eq!(post(&dev, "/keypress/Home").status, 200);
    assert_eq!(active(&dev).app_id, None);
}

#[test]
fn limited_post_is_refused_and_logged() {
    let dev = sim(DeviceKind::Express, SimTier::Limited);
    assert_eq!(post(&dev, "/launch/1002").status, 403);
    assert_eq!(active(&dev).app_id, None);
    assert_eq!(dev.launch_count(), 0);
    assert_eq!(
        dev.received_requests(),
        vec![
            ("POST".to_string(), "/launch/1002".to_string()),
            ("GET".to_string(), "/query/active-app".to_string()),
        ]
    );

    // "No POST arrived" is assertable from the log, not inferred.
    let quiet = sim(DeviceKind::Express, SimTier::Limited);
    let _ = get(&quiet, "/query/device-info");
    assert!(quiet.received_requests().iter().all(|(m, _)| m != "POST"));
}

// ---- 6. parsers round-trip ------------------------------------------------------

#[test]
fn proto_parsers_accept_sim_responses() {
    for (kind, model, is_tv) in [
        (DeviceKind::Express, "SIMX1", false),
        (DeviceKind::Tv, "SIMT1", true),
    ] {
        let dev = sim(kind, SimTier::Limited);
        let info = get(&dev, "/query/device-info");
        assert_eq!(info.status, 200);
        assert_eq!(
            info.header("Server"),
            Some("Roku/15.2.4 UPnP/1.0 Roku/15.2.4")
        );
        let parsed = parse_device_info(&info.body).expect("parse device-info");
        assert_eq!(parsed.model_number, model);
        assert_eq!(parsed.software_version, "15.2.4");
        assert_eq!(parsed.is_tv, is_tv);
        assert!(info.body.contains("FIXTURESERIAL"));

        let home = parse_active_app(&get(&dev, "/query/active-app").body).expect("home form");
        assert_eq!((home.app_id, home.name.as_str()), (None, "Roku"));
        assert_eq!(parse_ecp_response(&info.ecp()).gate, EcpGate::Open);

        let limited = get(&dev, "/query/apps");
        assert_eq!(parse_ecp_response(&limited.ecp()).gate, EcpGate::Limited403);
        assert_eq!(
            parse_ecp_response(&post(&dev, "/keypress/Home").ecp()).gate,
            EcpGate::Limited403
        );

        dev.set_tier(SimTier::Permissive);
        assert_eq!(
            parse_ecp_response(&get(&dev, "/query/apps").ecp()).gate,
            EcpGate::Open
        );
        assert_eq!(
            parse_ecp_response(&get(&dev, "/query/rid").ecp()).gate,
            EcpGate::Absent404
        );
    }
}

// ---- 7. suspend / resume -----------------------------------------------------------

#[test]
fn suspend_refuses_new_connections_and_resume_restores_service() {
    let mut dev = sim(DeviceKind::Express, SimTier::Permissive);
    let port = dev.port();
    assert_eq!(get(&dev, "/query/device-info").status, 200);

    dev.suspend();
    let err = TcpStream::connect(("127.0.0.1", port)).expect_err("suspended device must refuse");
    assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused);

    dev.resume();
    assert_eq!(dev.port(), port);
    assert_eq!(get(&dev, "/query/device-info").status, 200);
    // State and request log survive the fault.
    assert_eq!(dev.received_requests().len(), 2);
}
// ---- 8. latency fault ------------------------------------------------------------

#[test]
fn latency_fault_delays_but_preserves_exact_responses() {
    let dev = sim(DeviceKind::Express, SimTier::Permissive);
    dev.set_latency(300);
    let start = std::time::Instant::now();
    let r = get(&dev, "/query/device-info");
    assert!(
        start.elapsed() >= Duration::from_millis(250),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(r.status, 200);
    let parsed = parse_device_info(&r.body).expect("parse delayed device-info");
    assert_eq!(parsed.software_version, "15.2.4");
    assert!(r.body.contains("FIXTURESERIAL"));
    assert_eq!(
        dev.received_requests(),
        vec![("GET".to_string(), "/query/device-info".to_string())]
    );

    // Same transparency under Limited: the exact 403 survives the delay.
    dev.set_tier(SimTier::Limited);
    let start = std::time::Instant::now();
    let r = get(&dev, "/query/apps");
    assert!(start.elapsed() >= Duration::from_millis(250));
    assert_eq!(r.status, 403);
    assert_eq!(r.body, "ECP command not allowed in Limited mode.");
    assert_eq!(parse_ecp_response(&r.ecp()).gate, EcpGate::Limited403);

    // Clearing the fault restores immediate responses.
    dev.set_latency(0);
    let start = std::time::Instant::now();
    assert_eq!(get(&dev, "/query/device-info").status, 200);
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "{:?}",
        start.elapsed()
    );
}
