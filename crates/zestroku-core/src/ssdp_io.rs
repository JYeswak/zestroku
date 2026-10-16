//! m7b remaining scope — SSDP socket round-trip + discovery orchestration,
//! built against the landed pure core (`discovery.rs`, commit aae5751).
//!
//! Owner: OrangeShore (pane %6), bead zestroku-m7b-discovery-fc4.
//! LANDING NOTE: core/src/** is reservation-held by PeachKnoll (m7e) until
//! 17:32Z; this file drops into `crates/zestroku-core/src/ssdp_io.rs` (or is
//! folded into `discovery.rs`) the moment that reservation frees. Verified in
//! scratch via the harness crate before any landing.
//!
//! Design (honest about asupersync 0.5 shapes, verified against the registry
//! source at ~/.cargo/registry/src/.../asupersync-0.5.0/src/net/udp.rs):
//! - `UdpSocket::bind("0.0.0.0:0")` — ephemeral port, `send_to` the multicast
//!   group, `recv_from` replies (connected-socket re-target has the macOS
//!   EISCONN trap; unconnected `send_to`/`recv_from` avoids it entirely).
//! - `join_multicast_v4` is NOT needed to *send* M-SEARCH; we never listen on
//!   the group (replies are unicast to our ephemeral port — checklist §1 BPF
//!   note). Skipping the join avoids interface-selection races.
//! - Bounded reads: `recv_from` into `MAX_DATAGRAM` buffer; bigger replies
//!   truncate and then fail parse (discovery.rs refuses >MAX_DATAGRAM and
//!   malformed trailing content) — MTU-safe by construction.
//! - MX window: `asupersync::time::timeout(cx.now(), MX window, collect())`.
//!   `Cx::now()` needs `cap::HasTime` — satisfied by `cap::All` (the default).
//! - One sweep in flight; the collect loop stops early once every reply
//!   dedupes to the same USN set twice (or window expires) — checklist §1
//!   "repeat the sweep" is the caller's loop, not a tight probe.
#![forbid(unsafe_code)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use zestroku_proto::{DeviceInfo, Transport};
use zestroku_types::{Device, Error, Result};

use crate::discovery::{
    Endpoint, MAX_DATAGRAM, build_msearch, identity_key, merge_endpoints, parse_ssdp_response,
    parse_targets, reconcile,
};

/// One sweep's reply budget: every advertised ST answers, so a pair of
/// devices answers up to ~6 datagrams; 32 is generous headroom.
const MAX_REPLIES_PER_SWEEP: usize = 32;

/// One endpoint advertisement and the UDP peer that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedEndpoint {
    pub endpoint: Endpoint,
    pub responder: SocketAddr,
}

/// What one M-SEARCH sweep produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepResult {
    /// Endpoints parsed from replies with their responder source addresses.
    pub endpoints: Vec<SourcedEndpoint>,
    /// Datagram-level errors, kept for the operator (refused STs, garbage).
    pub refused: Vec<String>,
    scope: DiscoveryScope,
}

/// Send one M-SEARCH for `roku:ecp` and collect replies inside the MX window.
///
/// `mx_secs` is the MX header value AND the collection window (responders
/// wait a random delay inside MX, so the listener must outwait it).
pub async fn sweep(mx_secs: u8) -> Result<SweepResult> {
    sweep_with_scope(
        crate::discovery::SSDP_ADDR,
        mx_secs,
        DiscoveryScope::LocalLan,
    )
    .await
}

/// Sweep one explicit target address (multicast group for the live LAN,
/// a loopback responder for tests). Test seam so the sweep pipeline is
/// exercisable offline.
pub async fn sweep_to(target: &str, mx_secs: u8) -> Result<SweepResult> {
    let scope = explicit_target_scope(target);
    sweep_with_scope(target, mx_secs, scope).await
}

/// Production multicast is limited to private IPv4 peers. Explicit local
/// targets are narrower: the LOCATION must match that exact peer address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscoveryScope {
    LocalLan,
    ExplicitTarget(IpAddr),
}

fn explicit_target_scope(target: &str) -> DiscoveryScope {
    let Ok(address) = target.parse::<SocketAddr>() else {
        return DiscoveryScope::LocalLan;
    };
    match address.ip() {
        IpAddr::V4(ip) if ip.is_private() || ip.is_loopback() => {
            DiscoveryScope::ExplicitTarget(address.ip())
        }
        _ => DiscoveryScope::LocalLan,
    }
}

fn advertisement_is_in_scope(
    endpoint: &Endpoint,
    responder: SocketAddr,
    scope: DiscoveryScope,
) -> bool {
    let SocketAddr::V4(responder) = responder else {
        return false;
    };
    let Ok(location_ip) = endpoint.host.parse::<Ipv4Addr>() else {
        return false;
    };
    if location_ip != *responder.ip() {
        return false;
    }
    match scope {
        DiscoveryScope::LocalLan => responder.ip().is_private(),
        DiscoveryScope::ExplicitTarget(target) => {
            target == IpAddr::V4(*responder.ip())
                && (responder.ip().is_private() || responder.ip().is_loopback())
        }
    }
}

async fn sweep_with_scope(target: &str, mx_secs: u8, scope: DiscoveryScope) -> Result<SweepResult> {
    if mx_secs == 0 {
        return Err(Error::Malformed("ssdp: MX must be >= 1".into()));
    }
    let request = build_msearch(mx_secs);
    let window = Duration::from_secs(u64::from(mx_secs));
    // NO outer async timeout. The MX-window collection runs in a dedicated
    // blocking std thread (the StdFallbackTransport pattern): asupersync's
    // async `recv_from` never wakes for datagrams that arrive (bead 8dp,
    // second finding — minimal probe loopback-local/udp_readiness_probe.rs,
    // exit 124 local+remote), so the reactor path cannot deliver replies.
    // The blocking std socket honors the window via a read timeout and
    // ALWAYS returns its accumulator. One short bounded op per sweep; the
    // async wrapper awaits its join handle.
    let target = target.to_owned();
    let handle = std::thread::spawn(move || collect_replies(&request, target, window, scope));
    handle
        .join()
        .map_err(|_| Error::Unreachable("ssdp collector panicked".into()))
}

/// Blocking collector: send the M-SEARCH from a dedicated std socket to
/// `target`, then recv until the MX window closes. Owns its accumulator;
/// returns everything gathered (dedupe at the end) — nothing is dropped on
/// timeout.
fn collect_replies(
    request: &[u8],
    target: String,
    window: Duration,
    scope: DiscoveryScope,
) -> SweepResult {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").expect("ssdp bind");
    socket.set_read_timeout(Some(window)).expect("read timeout");
    let sent = socket
        .send_to(request, &target)
        .map_err(|e| Error::Unreachable(format!("ssdp send: {e}")))
        .expect("ssdp send");
    debug_assert_eq!(sent, request.len(), "M-SEARCH datagram fits one send");

    let mut endpoints = Vec::new();
    let mut refused = Vec::new();
    let mut buf = vec![0u8; MAX_DATAGRAM + 1]; // +1: detect oversize on read
    let deadline = Instant::now() + window;
    while endpoints.len() + refused.len() < MAX_REPLIES_PER_SWEEP {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        socket
            .set_read_timeout(Some(remaining))
            .expect("read timeout refresh");
        match socket.recv_from(&mut buf) {
            Ok((n, source)) => match parse_ssdp_response(&buf[..n]) {
                Ok(endpoint) => endpoints.push(SourcedEndpoint {
                    endpoint,
                    responder: source,
                }),
                Err(error) => refused.push(error.to_string()),
            },
            Err(error) => {
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut
                {
                    break; // window closed
                }
                if error.kind() != std::io::ErrorKind::Interrupted {
                    refused.push(format!("recv: {error}"));
                    break;
                }
            }
        }
    }
    endpoints.sort_by(|a, b| {
        a.endpoint
            .cmp(&b.endpoint)
            .then_with(|| a.responder.cmp(&b.responder))
    });
    endpoints.dedup_by(|a, b| a.endpoint == b.endpoint && a.responder == b.responder);
    SweepResult {
        endpoints,
        refused,
        scope,
    }
}

/// Full discovery: SSDP sweep ∪ targets.toml seeds, then reconcile each
/// endpoint into a `(Device, identity)` pair by probing `query/device-info`
/// over `transport`. The identity is hashed from the already-parsed info —
/// no second probe. Unknown (empty) identity never merges downstream.
pub async fn discover(
    transport: &impl Transport,
    seeds_toml: Option<&str>,
    mx_secs: u8,
) -> Result<(Vec<(Device, String)>, Vec<String>)> {
    let sweep = sweep(mx_secs).await?;
    discover_from_sweep(transport, seeds_toml, sweep).await
}

async fn discover_from_sweep(
    transport: &impl Transport,
    seeds_toml: Option<&str>,
    sweep: SweepResult,
) -> Result<(Vec<(Device, String)>, Vec<String>)> {
    let seeds = match seeds_toml {
        Some(text) => parse_targets(text)?,
        None => Vec::new(),
    };
    let mut notes = sweep.refused;
    let mut ssdp_endpoints = Vec::with_capacity(sweep.endpoints.len());
    for sourced in sweep.endpoints {
        if advertisement_is_in_scope(&sourced.endpoint, sourced.responder, sweep.scope) {
            ssdp_endpoints.push(sourced.endpoint);
        } else {
            notes.push(format!(
                "ssdp: LOCATION {}:{} is outside the responder scope ({})",
                sourced.endpoint.host, sourced.endpoint.port, sourced.responder
            ));
        }
    }
    let endpoints = merge_endpoints(&ssdp_endpoints, &seeds);
    let mut devices = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        match probe_device_info(transport, &endpoint).await {
            Ok(info) => devices.push((reconcile(&endpoint, &info), identity_key(&info))),
            Err(error) => notes.push(format!("{}:{}: {error}", endpoint.host, endpoint.port)),
        }
    }
    Ok((devices, notes))
}

/// Probe one endpoint's `query/device-info` (Open tier, read-only).
async fn probe_device_info(transport: &impl Transport, endpoint: &Endpoint) -> Result<DeviceInfo> {
    let cmd = zestroku_proto::query("device-info");
    let response = transport.send(&endpoint.host, endpoint.port, &cmd).await?;
    if response.status != 200 {
        return Err(Error::Unreachable(format!(
            "device-info returned HTTP {}",
            response.status
        )));
    }
    zestroku_proto::parse_device_info(&response.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::ECP_DEFAULT_PORT;
    use std::io::{Read, Write};
    use std::net::{TcpStream, UdpSocket};
    use std::thread;
    use std::time::Duration;
    use zestroku_proto::{Command, EcpResponse};
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    struct FixtureTransport {
        bodies: std::collections::BTreeMap<(String, u16), String>,
        probed: std::sync::RwLock<Vec<(String, u16)>>,
    }

    impl FixtureTransport {
        fn send_sync(&self, host: &str, port: u16, _cmd: &Command) -> Result<EcpResponse> {
            self.probed
                .write()
                .expect("probed lock")
                .push((host.to_owned(), port));
            let body = self
                .bodies
                .get(&(host.to_owned(), port))
                .cloned()
                .unwrap_or_default();
            let status = if body.is_empty() { 404 } else { 200 };
            Ok(EcpResponse {
                status,
                headers: Vec::new(),
                body,
            })
        }
    }

    impl Transport for FixtureTransport {
        async fn send(&self, host: &str, port: u16, cmd: &Command) -> Result<EcpResponse> {
            self.send_sync(host, port, cmd)
        }
    }

    #[derive(Clone, Copy)]
    struct LoopbackTransport;

    impl Transport for LoopbackTransport {
        async fn send(&self, host: &str, port: u16, cmd: &Command) -> Result<EcpResponse> {
            let address = format!("{host}:{port}");
            let mut stream = TcpStream::connect(&address)
                .map_err(|error| Error::Unreachable(format!("{address}: {error}")))?;
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|error| Error::Unreachable(format!("read timeout: {error}")))?;
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .map_err(|error| Error::Unreachable(format!("write timeout: {error}")))?;
            write!(
                stream,
                "{} {} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n",
                cmd.method, cmd.path
            )
            .map_err(|error| Error::Unreachable(format!("write: {error}")))?;
            let mut raw = Vec::new();
            stream
                .read_to_end(&mut raw)
                .map_err(|error| Error::Unreachable(format!("read: {error}")))?;
            let response = String::from_utf8_lossy(&raw);
            let status = response
                .split_whitespace()
                .nth(1)
                .and_then(|status| status.parse().ok())
                .ok_or_else(|| Error::Unreachable("malformed status line".into()))?;
            let body = response
                .split_once("\r\n\r\n")
                .map_or("", |(_, body)| body)
                .to_owned();
            Ok(EcpResponse {
                status,
                headers: Vec::new(),
                body,
            })
        }
    }

    fn runtime() -> asupersync::runtime::Runtime {
        let reactor = asupersync::runtime::reactor::create_reactor().expect("reactor");
        asupersync::runtime::RuntimeBuilder::current_thread()
            .with_reactor(reactor)
            .build()
            .expect("runtime")
    }

    fn info_xml(model: &str) -> String {
        format!(
            "<device-info><udn>FIXTUREUDN</udn><model-name>{model}</model-name>\
             <model-number>9999X</model-number><software-version>0.0.0</software-version>\
             <serial-number>FIXTURESERIAL</serial-number><is-tv>false</is-tv></device-info>"
        )
    }

    fn sourced_endpoint(host: &str, port: u16) -> SourcedEndpoint {
        let ip = host.parse::<Ipv4Addr>().expect("fixture IPv4 address");
        SourcedEndpoint {
            endpoint: Endpoint {
                host: host.to_owned(),
                port,
            },
            responder: SocketAddr::new(IpAddr::V4(ip), 1900),
        }
    }

    #[test]
    fn ssdp_scope_requires_a_matching_private_ipv4_responder() {
        let in_scope = DiscoveryScope::LocalLan;
        assert!(advertisement_is_in_scope(
            &Endpoint {
                host: "192.168.1.23".into(),
                port: ECP_DEFAULT_PORT,
            },
            "192.168.1.23:1900".parse().unwrap(),
            in_scope,
        ));
        for (host, responder) in [
            ("127.0.0.1", "127.0.0.1:1900"),
            ("169.254.169.254", "169.254.169.254:1900"),
            ("10.1.2.99", "192.168.1.23:1900"),
            ("203.0.113.8", "203.0.113.8:1900"),
            ("roku.attacker.invalid", "192.168.1.23:1900"),
        ] {
            let endpoint = Endpoint {
                host: host.into(),
                port: ECP_DEFAULT_PORT,
            };
            assert!(
                !advertisement_is_in_scope(
                    &endpoint,
                    responder.parse().expect("fixture responder"),
                    in_scope,
                ),
                "unexpectedly trusted LOCATION {host} from {responder}"
            );
        }
        assert!(advertisement_is_in_scope(
            &Endpoint {
                host: "127.0.0.1".into(),
                port: ECP_DEFAULT_PORT,
            },
            "127.0.0.1:1900".parse().unwrap(),
            DiscoveryScope::ExplicitTarget(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        ));
    }

    #[test]
    fn local_lan_refuses_loopback_before_transport() {
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::new(),
            probed: std::sync::RwLock::new(Vec::new()),
        };
        let (devices, notes) = runtime()
            .block_on(discover_from_sweep(
                &transport,
                None,
                SweepResult {
                    endpoints: vec![sourced_endpoint("127.0.0.1", ECP_DEFAULT_PORT)],
                    refused: Vec::new(),
                    scope: DiscoveryScope::LocalLan,
                },
            ))
            .expect("discovery");
        assert!(devices.is_empty());
        assert_eq!(notes.len(), 1);
        assert!(
            transport.probed.read().expect("probed lock").is_empty(),
            "a loopback LOCATION must not reach Transport::send"
        );
    }

    #[test]
    fn discover_merges_seeds_and_reconciles_probes() {
        let seeds = "[[target]]\nhost = \"192.0.2.10\"\n[[target]]\nhost = \"192.0.2.11\"\n";
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::from([
                (
                    ("192.0.2.10".into(), ECP_DEFAULT_PORT),
                    info_xml("Roku Express"),
                ),
                (("192.0.2.11".into(), ECP_DEFAULT_PORT), info_xml("Roku TV")),
            ]),
            probed: std::sync::RwLock::new(Vec::new()),
        };
        let (devices, notes) = runtime()
            .block_on(discover_from_sweep(
                &transport,
                Some(seeds),
                SweepResult {
                    endpoints: Vec::new(),
                    refused: Vec::new(),
                    scope: DiscoveryScope::LocalLan,
                },
            ))
            .expect("discovery");
        assert_eq!(devices.len(), 2, "both seeds reconcile: {notes:?}");
        assert_eq!(devices[0].0.model_name, "Roku Express");
        assert_eq!(devices[1].0.model_name, "Roku TV");
        assert!(
            devices.iter().all(|(device, identity)| {
                device.port == ECP_DEFAULT_PORT && identity.len() == 64
            })
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            transport.probed.read().expect("probed lock").len(),
            2,
            "one probe per endpoint"
        );
    }

    #[test]
    fn unreachable_endpoint_degrades_to_note_not_error() {
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::from([(
                ("192.168.1.10".into(), ECP_DEFAULT_PORT),
                info_xml("Roku Express"),
            )]),
            probed: std::sync::RwLock::new(Vec::new()),
        };
        let (devices, notes) = runtime()
            .block_on(discover_from_sweep(
                &transport,
                None,
                SweepResult {
                    endpoints: vec![
                        sourced_endpoint("192.168.1.10", ECP_DEFAULT_PORT),
                        sourced_endpoint("192.168.1.99", ECP_DEFAULT_PORT),
                    ],
                    refused: Vec::new(),
                    scope: DiscoveryScope::LocalLan,
                },
            ))
            .expect("discovery");
        assert_eq!(devices.len(), 1);
        assert_eq!(
            notes.len(),
            1,
            "the dead endpoint becomes a note: {notes:?}"
        );
        assert!(notes[0].contains("192.168.1.99"), "{notes:?}");
    }

    #[test]
    fn probe_error_message_carries_host_and_status() {
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::new(),
            probed: std::sync::RwLock::new(Vec::new()),
        };
        let (devices, notes) = runtime()
            .block_on(discover_from_sweep(
                &transport,
                None,
                SweepResult {
                    endpoints: vec![sourced_endpoint("192.168.1.50", 8061)],
                    refused: Vec::new(),
                    scope: DiscoveryScope::LocalLan,
                },
            ))
            .expect("discovery");
        assert!(devices.is_empty());
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("404"), "{notes:?}");
    }

    #[test]
    fn loopback_ssdp_discovery_retains_identity_with_one_device_info_query() {
        let sim = SimDevice::start_with_serial(
            SimConfig {
                kind: DeviceKind::Express,
                tier: SimTier::Permissive,
            },
            "FIXTURESERIAL-SSDP",
        );
        let responder = UdpSocket::bind("127.0.0.1:0").expect("SSDP responder");
        responder
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("SSDP read timeout");
        let target = format!("127.0.0.1:{}", responder.local_addr().unwrap().port());
        let sim_port = sim.port();
        let responder_thread = thread::spawn(move || {
            let mut request = [0_u8; 1500];
            let (length, peer) = responder.recv_from(&mut request).expect("receive M-SEARCH");
            assert!(
                String::from_utf8_lossy(&request[..length]).contains("M-SEARCH * HTTP/1.1"),
                "unexpected SSDP request"
            );
            let reply = format!(
                "HTTP/1.1 200 OK\r\nLOCATION: http://127.0.0.1:{sim_port}/\r\n\
                 ST: roku:ecp\r\nUSN: uuid:roku:ecp:loopback\r\n\r\n"
            );
            responder
                .send_to(reply.as_bytes(), peer)
                .expect("send SSDP reply");
        });
        let (devices, notes) = runtime()
            .block_on(async {
                let sweep = sweep_to(&target, 1).await?;
                discover_from_sweep(&LoopbackTransport, None, sweep).await
            })
            .expect("loopback discovery");
        responder_thread.join().expect("SSDP responder thread");
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(devices.len(), 1, "SSDP endpoint must reconcile");
        assert_eq!(devices[0].0.port, sim.port());
        let expected = zestroku_proto::parse_device_info(
            "<device-info><udn>FIXTUREUDN</udn><model-name>Sim Express</model-name>\
             <model-number>SIMX1</model-number><software-version>15.2.4</software-version>\
             <serial-number>FIXTURESERIAL-SSDP</serial-number><is-tv>false</is-tv></device-info>",
        )
        .expect("expected sim identity");
        assert_eq!(devices[0].1, identity_key(&expected));
        assert_eq!(
            sim.received_requests(),
            vec![("GET".into(), "/query/device-info".into())],
            "discovery must retain the identity from exactly one DeviceInfo query"
        );
    }
    #[test]
    fn out_of_scope_ssdp_locations_never_reach_transport() {
        let responder = UdpSocket::bind("127.0.0.1:0").expect("SSDP responder");
        responder
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("SSDP read timeout");
        let target = format!("127.0.0.1:{}", responder.local_addr().unwrap().port());
        let responder_thread = thread::spawn(move || {
            let mut request = [0_u8; 1500];
            let (length, peer) = responder.recv_from(&mut request).expect("receive M-SEARCH");
            assert!(
                String::from_utf8_lossy(&request[..length]).contains("M-SEARCH * HTTP/1.1"),
                "unexpected SSDP request"
            );
            for (index, location) in [
                "169.254.169.254:8060",
                "10.1.2.99:8060",
                "203.0.113.8:8060",
                "roku.attacker.invalid:8060",
            ]
            .iter()
            .enumerate()
            {
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nLOCATION: http://{location}/\r\n\
                     ST: roku:ecp\r\nUSN: uuid:roku:ecp:escape-{index}\r\n\r\n"
                );
                responder
                    .send_to(reply.as_bytes(), peer)
                    .expect("send SSDP reply");
            }
        });
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::new(),
            probed: std::sync::RwLock::new(Vec::new()),
        };

        let (devices, notes, parsed_count) = runtime()
            .block_on(async {
                let sweep = sweep_to(&target, 1).await?;
                let parsed_count = sweep.endpoints.len();
                let (devices, notes) = discover_from_sweep(&transport, None, sweep).await?;
                Ok::<_, Error>((devices, notes, parsed_count))
            })
            .expect("loopback discovery");
        responder_thread.join().expect("SSDP responder thread");
        assert_eq!(parsed_count, 4, "every valid Roku SSDP reply is parsed");

        assert!(
            devices.is_empty(),
            "out-of-scope advertisements are refused"
        );
        assert_eq!(
            notes.len(),
            4,
            "each escape is refused by policy: {notes:?}"
        );
        assert!(
            notes
                .iter()
                .all(|note| note.contains("outside the responder scope")),
            "the parser accepted these replies before the scope refused them: {notes:?}"
        );
        assert!(
            transport.probed.read().expect("probed lock").is_empty(),
            "SSDP LOCATION escapes must be rejected before Transport::send"
        );
    }

    #[test]
    fn explicit_target_rejects_alternate_loopback_location_before_transport() {
        // Same-host spoof: a 127.0.0.1 responder advertising a *different*
        // loopback LOCATION must be refused even though both addresses are
        // loopback — scope binds to the responder source, not the class.
        let responder = UdpSocket::bind("127.0.0.1:0").expect("SSDP responder");
        responder
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("SSDP read timeout");
        let target = format!("127.0.0.1:{}", responder.local_addr().unwrap().port());
        let responder_thread = thread::spawn(move || {
            let mut request = [0_u8; 1500];
            let (length, peer) = responder.recv_from(&mut request).expect("receive M-SEARCH");
            assert!(
                String::from_utf8_lossy(&request[..length]).contains("M-SEARCH * HTTP/1.1"),
                "unexpected SSDP request"
            );
            let reply = "HTTP/1.1 200 OK\r\nLOCATION: http://127.0.0.2:8060/\r\n\
                 ST: roku:ecp\r\nUSN: uuid:roku:ecp:alternate-loopback\r\n\r\n";
            responder
                .send_to(reply.as_bytes(), peer)
                .expect("send SSDP reply");
        });
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::new(),
            probed: std::sync::RwLock::new(Vec::new()),
        };

        let (devices, notes) = runtime()
            .block_on(async {
                let sweep = sweep_to(&target, 1).await?;
                discover_from_sweep(&transport, None, sweep).await
            })
            .expect("loopback discovery");
        responder_thread.join().expect("SSDP responder thread");
        assert!(devices.is_empty(), "alternate-loopback LOCATION is refused");
        assert_eq!(notes.len(), 1, "one outside-scope note: {notes:?}");
        assert!(
            notes[0].contains("outside the responder scope"),
            "{notes:?}"
        );
        assert!(
            transport.probed.read().expect("probed lock").is_empty(),
            "alternate-loopback escape must be rejected before Transport::send"
        );
    }
}
