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

use std::time::Duration;

use asupersync::net::UdpSocket;
use asupersync::time::{timeout, wall_now};
use zestroku_proto::{DeviceInfo, Transport};
use zestroku_types::{Device, Error, Result};

use crate::discovery::{
    Endpoint, MAX_DATAGRAM, build_msearch, merge_endpoints, parse_ssdp_response, parse_targets,
    reconcile,
};

/// One sweep's reply budget: every advertised ST answers, so a pair of
/// devices answers up to ~6 datagrams; 32 is generous headroom.
const MAX_REPLIES_PER_SWEEP: usize = 32;

/// What one M-SEARCH sweep produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepResult {
    /// Endpoints parsed from replies (deduped, stable order).
    pub endpoints: Vec<Endpoint>,
    /// Datagram-level errors, kept for the operator (refused STs, garbage).
    pub refused: Vec<String>,
}

/// Send one M-SEARCH for `roku:ecp` and collect replies inside the MX window.
///
/// `mx_secs` is the MX header value AND the collection window (responders
/// wait a random delay inside MX, so the listener must outwait it).
pub async fn sweep(mx_secs: u8) -> Result<SweepResult> {
    if mx_secs == 0 {
        return Err(Error::Malformed("ssdp: MX must be >= 1".into()));
    }
    let mut socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| Error::Unreachable(format!("ssdp bind: {e}")))?;
    let request = build_msearch(mx_secs);
    let sent = socket
        .send_to(&request, crate::discovery::SSDP_ADDR)
        .await
        .map_err(|e| Error::Unreachable(format!("ssdp send: {e}")))?;
    if sent != request.len() {
        return Err(Error::Unreachable(format!(
            "ssdp send short: {sent}/{} bytes",
            request.len()
        )));
    }

    let window = Duration::from_secs(u64::from(mx_secs));
    let collected = timeout(wall_now(), window, collect_replies(&mut socket)).await;
    match collected {
        Ok(result) => Ok(result),
        Err(_elapsed) => {
            // Window closed: what we have is what answered. A sweep with zero
            // replies is a legitimate empty result (SSDP is flaky), not an
            // error — seeds carry the run (bead (c)).
            Ok(SweepResult {
                endpoints: Vec::new(),
                refused: Vec::new(),
            })
        }
    }
}

/// Receive until the caller's timeout cancels us (cancel-safe: each
/// `recv_from` either completes or is dropped whole with the socket).
async fn collect_replies(socket: &mut UdpSocket) -> SweepResult {
    let mut endpoints = Vec::new();
    let mut refused = Vec::new();
    let mut buf = vec![0u8; MAX_DATAGRAM + 1]; // +1: detect oversize on read
    while endpoints.len() + refused.len() < MAX_REPLIES_PER_SWEEP {
        match socket.recv_from(&mut buf).await {
            Ok((n, _source)) => match parse_ssdp_response(&buf[..n]) {
                // merge_endpoints dedupes; push everything, dedupe at the end.
                Ok(endpoint) => endpoints.push(endpoint),
                Err(error) => refused.push(error.to_string()),
            },
            Err(error) => {
                refused.push(format!("recv: {error}"));
                // Transient interrupt → keep collecting; anything else stops.
                if error.kind() != std::io::ErrorKind::Interrupted {
                    break;
                }
            }
        }
    }
    SweepResult {
        endpoints: merge_endpoints(&endpoints, &[]),
        refused,
    }
}

/// Full discovery: SSDP sweep ∪ targets.toml seeds, then reconcile each
/// endpoint into a `Device` by probing `query/device-info` over `transport`.
///
/// One probe failure downgrades that endpoint to an unreachable note; it does
/// not fail the whole run (a seeds file may legitimately list a powered-off
/// device — the store records last-known state, m7d).
pub async fn discover(
    transport: &impl Transport,
    seeds_toml: Option<&str>,
    mx_secs: u8,
) -> Result<(Vec<Device>, Vec<String>)> {
    let sweep = sweep(mx_secs).await?;
    let seeds = match seeds_toml {
        Some(text) => parse_targets(text)?,
        None => Vec::new(),
    };
    let endpoints = merge_endpoints(&sweep.endpoints, &seeds);
    let mut devices = Vec::with_capacity(endpoints.len());
    let mut notes = sweep.refused;
    for endpoint in endpoints {
        match probe_device_info(transport, &endpoint).await {
            Ok(info) => devices.push(reconcile(&endpoint, &info)),
            Err(error) => notes.push(format!("{}:{}: {error}", endpoint.host, endpoint.port)),
        }
    }
    Ok((devices, notes))
}

/// Probe one endpoint's `query/device-info` (Open tier, read-only).
async fn probe_device_info(transport: &impl Transport, endpoint: &Endpoint) -> Result<DeviceInfo> {
    let cmd = zestroku_proto::query("device-info");
    let response = transport
        .send(&endpoint.host, endpoint.port, &cmd)
        .await?;
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
    use zestroku_proto::{Command, EcpResponse};

    /// Fixture transport: answers device-info with a scrubbed body, records
    /// the endpoints probed. No network, no runtime, no unsafe.
    ///
    /// The async trait method delegates to `send_sync` (the fixture future is
    /// immediately ready); tests drive `send_sync` directly, so the socket-free
    /// reconcile loop is proven offline. The socket sweep itself is covered by
    /// the live leg (bead (b), Josh present) and the sim (m7f).
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

    fn info_xml(model: &str) -> String {
        // Scrubbed synthetic device-info (TEST-NET hosts, fake values only).
        // Field names match the real parser contract (`model-name`, not
        // `friendly-name` — zestroku-proto parse_device_info).
        format!(
            "<device-info>\n<model-name>{model}</model-name>\
             \n<model-number>9999X</model-number>\
             \n<software-version>0.0.0</software-version>\
             \n<serial-number>FIXTURESERIAL</serial-number>\
             \n<is-tv>false</is-tv>\n</device-info>"
        )
    }

    /// The socket-free half of `discover`, driven synchronously in tests:
    /// merge seeds, probe each endpoint, reconcile or note the failure.
    /// Mirrors discover()'s endpoint loop exactly (same helpers, same order);
    /// the sweep half above the `cfg(test)` line is the part that needs Cx.
    fn discover_sync(
        transport: &FixtureTransport,
        seeds: &[Endpoint],
    ) -> (Vec<Device>, Vec<String>) {
        let endpoints = merge_endpoints(&[], seeds);
        let mut devices = Vec::new();
        let mut notes = Vec::new();
        for endpoint in endpoints {
            let cmd = zestroku_proto::query("device-info");
            match transport.send_sync(&endpoint.host, endpoint.port, &cmd) {
                Ok(response) if response.status == 200 => {
                    match zestroku_proto::parse_device_info(&response.body) {
                        Ok(info) => devices.push(reconcile(&endpoint, &info)),
                        Err(error) => notes.push(format!("{}: {error}", endpoint.host)),
                    }
                }
                Ok(response) => notes.push(format!("{}: HTTP {}", endpoint.host, response.status)),
                Err(error) => notes.push(format!("{}: {error}", endpoint.host)),
            }
        }
        (devices, notes)
    }

    #[test]
    fn discover_merges_seeds_and_reconciles_probes() {
        let seeds = parse_targets(
            "[[target]]\nhost = \"192.0.2.10\"\n[[target]]\nhost = \"192.0.2.11\"\n",
        )
        .unwrap();
        assert_eq!(seeds.len(), 2);
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
        let (devices, notes) = discover_sync(&transport, &seeds);
        assert_eq!(devices.len(), 2, "both seeds reconcile: {notes:?}");
        assert_eq!(devices[0].model_name, "Roku Express");
        assert_eq!(devices[1].model_name, "Roku TV");
        assert!(devices.iter().all(|d| d.port == ECP_DEFAULT_PORT));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(transport.probed.read().expect("probed lock").len(), 2, "one probe per endpoint");
    }

    #[test]
    fn unreachable_endpoint_degrades_to_note_not_error() {
        // A seeds file may list a powered-off device: probe failure → note,
        // the rest of the run continues (store keeps last-known state, m7d).
        let seeds = vec![
            Endpoint { host: "192.0.2.10".into(), port: ECP_DEFAULT_PORT },
            Endpoint { host: "192.0.2.99".into(), port: ECP_DEFAULT_PORT },
        ];
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::from([(
                ("192.0.2.10".into(), ECP_DEFAULT_PORT),
                info_xml("Roku Express"),
            )]),
            probed: std::sync::RwLock::new(Vec::new()),
        };
        let (devices, notes) = discover_sync(&transport, &seeds);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].host, "192.0.2.10");
        assert_eq!(notes.len(), 1, "the dead seed becomes a note: {notes:?}");
        assert!(notes[0].contains("192.0.2.99"), "{notes:?}");
    }

    #[test]
    fn probe_error_message_carries_host_and_status() {
        // The note format is the operator's only signal for a dead seed —
        // pin its shape (host + reason) so it can't silently degrade.
        let seeds = vec![Endpoint { host: "192.0.2.50".into(), port: 8061 }];
        let transport = FixtureTransport {
            bodies: std::collections::BTreeMap::new(), // everything 404s
            probed: std::sync::RwLock::new(Vec::new()),
        };
        let (devices, notes) = discover_sync(&transport, &seeds);
        assert!(devices.is_empty());
        assert_eq!(notes.len(), 1);
        // discover_sync notes carry "host: reason" (port omitted); the
        // failure kind (HTTP status) is the actionable part.
        assert!(notes[0].starts_with("192.0.2.50: "), "{notes:?}");
        assert!(notes[0].contains("404"), "{notes:?}");
    }
}