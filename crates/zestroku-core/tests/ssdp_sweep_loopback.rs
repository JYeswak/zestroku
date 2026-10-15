//! Loopback regression for the SSDP sweep (m7b, PeachKnoll finding 2026-10-10).
//!
//! A std-`UdpSocket` responder thread answers the M-SEARCH with two valid
//! `roku:ecp` replies (to the sender's ephemeral port). The sweep must return
//! BOTH endpoints.
//!
//! Planted negative (run against db9bf8e as-is): the outer
//! `timeout(wall_now(), window, collect_replies(...))` races the collector's
//! internal deadline. When the outer deadline wins the race, the cancelled
//! future's accumulator is DROPPED and the `Err(_elapsed)` arm synthesizes an
//! EMPTY result — so this test goes red (`endpoints: 0, expected 2`).
//!
//! Loopback-only: binds 127.0.0.1, no LAN, no multicast — the sweep targets
//! the responder's address directly via `sweep_to`.
#![forbid(unsafe_code)]

use std::net::UdpSocket;
use std::sync::mpsc;
use std::time::Duration;

use zestroku_core::discovery::{build_msearch, parse_ssdp_response};
use zestroku_core::ssdp_io::sweep_to;

/// Responder: recv one M-SEARCH, then send `replies` datagrams back to the
/// sender. Exits after the batch (the channel tells the test it's done).
fn spawn_responder(replies: Vec<Result<Vec<u8>, ()>>) -> (u16, mpsc::Receiver<()>) {
    let sock = UdpSocket::bind("127.0.0.1:0").expect("responder bind");
    let port = sock.local_addr().unwrap().port();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1500];
        // Wait for the M-SEARCH (any datagram; loopback, trusted).
        let (n, src) = sock.recv_from(&mut buf).expect("responder recv");
        assert!(String::from_utf8_lossy(&buf[..n]).contains("M-SEARCH"));
        for r in replies {
            match r {
                Ok(payload) => {
                    sock.send_to(&payload, src).expect("responder send");
                }
                Err(()) => { /* reserved for malformed-reply cases */ }
            }
        }
        done_tx.send(()).expect("responder done");
    });
    (port, done_rx)
}

fn reply(st: &str, host: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nLOCATION: http://{host}/\r\nST: {st}\r\nUSN: uuid:roku:ecp:FIXTURESERIAL\r\n\r\n"
    )
    .into_bytes()
}

#[test]
fn sweep_to_returns_both_replies_within_mx_window() {
    let (port, done) = spawn_responder(vec![
        Ok(reply("roku:ecp", "127.0.0.1:9001")),
        Ok(reply("roku:ecp", "127.0.0.1:9002")),
    ]);
    let rt = asupersync::runtime::reactor::create_reactor()
        .map_err(|e| std::io::Error::other(e.to_string()))
        .and_then(|reactor| {
            asupersync::runtime::RuntimeBuilder::current_thread()
                .with_reactor(reactor)
                .build()
                .map_err(|e| std::io::Error::other(e.to_string()))
        })
        .expect("runtime with reactor");
    let result = rt.block_on(async { sweep_to(&format!("127.0.0.1:{port}"), 1).await });
    let sweep = result.expect("sweep ok");
    // The responder can complete before we join, but give it its turn.
    let _ = done.recv_timeout(Duration::from_secs(2));
    assert_eq!(
        sweep.endpoints.len(),
        2,
        "BOTH in-window replies must survive: {:?} refused={:?}",
        sweep.endpoints,
        sweep.refused
    );
    assert!(
        sweep
            .endpoints
            .iter()
            .any(|e| e.endpoint.host == "127.0.0.1" && e.endpoint.port == 9001)
    );
    assert!(
        sweep
            .endpoints
            .iter()
            .any(|e| e.endpoint.host == "127.0.0.1" && e.endpoint.port == 9002)
    );
    assert!(
        sweep
            .endpoints
            .iter()
            .all(|endpoint| endpoint.responder.ip().is_loopback())
    );
}

#[test]
fn sweep_to_refuses_non_roku_st_and_keeps_valid_one() {
    let (port, done) = spawn_responder(vec![
        Ok(reply(
            "urn:dial-multiscreen-org:service:dial:1",
            "127.0.0.1:9003",
        )),
        Ok(reply("roku:ecp", "127.0.0.1:9004")),
    ]);
    let rt = asupersync::runtime::reactor::create_reactor()
        .map_err(|e| std::io::Error::other(e.to_string()))
        .and_then(|reactor| {
            asupersync::runtime::RuntimeBuilder::current_thread()
                .with_reactor(reactor)
                .build()
                .map_err(|e| std::io::Error::other(e.to_string()))
        })
        .expect("runtime with reactor");
    let result = rt.block_on(async { sweep_to(&format!("127.0.0.1:{port}"), 1).await });
    let sweep = result.expect("sweep ok");
    let _ = done.recv_timeout(Duration::from_secs(2));
    assert_eq!(
        sweep.endpoints.len(),
        1,
        "only roku:ecp endpoints pass: {:?}",
        sweep.endpoints
    );
    assert_eq!(sweep.endpoints[0].endpoint.port, 9004);
    assert_eq!(
        sweep.refused.len(),
        1,
        "dial ST counted as refused: {:?}",
        sweep.refused
    );
}

#[test]
fn msearch_sent_to_explicit_target_carries_roku_st() {
    // sweep_to must still put a well-formed M-SEARCH on the wire.
    let req = build_msearch(1);
    let s = String::from_utf8(req).unwrap();
    assert!(s.contains("ST: roku:ecp\r\n"));
    // And the parser still accepts a well-formed reply (guard for the
    // fixtures above being valid against the production parser).
    parse_ssdp_response(&reply("roku:ecp", "127.0.0.1:9050")).expect("fixture reply parses");
}
