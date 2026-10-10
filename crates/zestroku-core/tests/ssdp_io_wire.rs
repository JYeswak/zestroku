//! m7b wire proof: the full SSDP path through real fixture data —
//! M-SEARCH bytes → parsed reply → endpoint → device-info probe → Device —
//! plus the bead-(d) planted negative run live (non-roku ST refused, and the
//! refusal demonstrably depends on the ST check).
//!
//! No network: every network-shaped object here is a fixture (scrubbed,
//! TEST-NET hosts). The UDP socket path itself is exercised by the live leg
//! (bead (b), Josh present) — this file pins the wire FORMAT and the
//! reconcile pipeline that consumes it.
#![forbid(unsafe_code)]

use zestroku_core::discovery::{
    Endpoint, build_msearch, merge_endpoints, parse_ssdp_response, parse_targets, reconcile,
};
use zestroku_proto::{Command, EcpResponse, Transport};
use zestroku_types::Result;

/// A reply fixture with a synthetic (TEST-NET) LOCATION and fake serial.
const REPLY_OK: &[u8] = b"HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\n\
      LOCATION: http://192.0.2.10:8060/\r\n\
      SERVER: Roku/9.10.0 UPnP/1.0 Roku/9.10.0\r\n\
      ST: roku:ecp\r\nUSN: uuid:roku:ecp:FIXTURESERIAL1\r\n\r\n";

const REPLY_TV: &[u8] = b"HTTP/1.1 200 OK\r\nLOCATION: http://192.0.2.11:8060/\r\n\
      ST: roku:ecp\r\nUSN: uuid:roku:ecp:FIXTURESERIAL2\r\n\r\n";

/// Same reply with the DIAL ST a Roku also advertises (ECP-WIRE-DOC §1) —
/// the parser must refuse it: zestroku acts on `roku:ecp` only (bead (d)).
const REPLY_DIAL_ST: &[u8] = b"HTTP/1.1 200 OK\r\nLOCATION: http://192.0.2.10:8060/\r\n\
      ST: urn:dial-multiscreen-org:service:dial:1\r\n\
      USN: uuid:roku:ecp:FIXTURESERIAL1\r\n\r\n";

#[test]
fn msearch_bytes_match_the_wire_contract() {
    let req = build_msearch(3);
    let s = std::str::from_utf8(&req).unwrap();
    assert!(s.starts_with("M-SEARCH * HTTP/1.1\r\n"));
    assert!(
        s.contains("HOST: 239.255.255.250:1900\r\n"),
        "multicast endpoint"
    );
    assert!(s.contains("MAN: \"ssdp:discover\"\r\n"), "quoted MAN");
    assert!(s.contains("MX: 3\r\n"));
    assert!(s.contains("ST: roku:ecp\r\n"));
    assert!(s.ends_with("\r\n\r\n"), "blank-line terminator");
    assert!(req.len() < 512, "MTU-safe datagram, got {}B", req.len());
}

#[test]
fn replies_parse_to_endpoints_and_merge_with_seeds() {
    let a = parse_ssdp_response(REPLY_OK).expect("express reply parses");
    let b = parse_ssdp_response(REPLY_TV).expect("tv reply parses");
    assert_eq!(
        a,
        Endpoint {
            host: "192.0.2.10".into(),
            port: 8060
        }
    );
    assert_eq!(b.port, 8060);

    // Seeds (multicast black-holed fallback) alone must cover every device:
    let seeds =
        parse_targets("[[target]]\nhost = \"192.0.2.10\"\n[[target]]\nhost = \"192.0.2.11\"\n")
            .expect("targets parse");
    assert_eq!(merge_endpoints(&[], &seeds).len(), 2, "seeds-only run");
    // Sweep + seeds: deduped union, stable order.
    assert_eq!(merge_endpoints(&[a, b], &seeds).len(), 2);
}

#[test]
fn planted_negative_non_roku_st_is_refused() {
    // Bead (d): the parser must refuse a non-roku:ecp ST. Deleting the ST
    // check in parse_ssdp_response turns THIS test red (the DIAL reply would
    // parse into an endpoint instead of erroring).
    let err = parse_ssdp_response(REPLY_DIAL_ST).expect_err("dial ST must be refused");
    assert!(
        err.to_string().contains("ST"),
        "error names the ST gate: {err}"
    );
}

/// Fixture transport over the real `Transport` seam (as m7a defined it):
/// device-info answers with scrubbed XML; anything else 404s.
struct WireTransport;

impl WireTransport {
    /// Sync core of the probe, driven directly by tests (the async trait
    /// method delegates here; fixture futures are immediately ready).
    fn send_sync(&self, host: &str, _port: u16, cmd: &Command) -> EcpResponse {
        if cmd.path == "/query/device-info" {
            let model = if host == "192.0.2.11" {
                "Roku TV"
            } else {
                "Roku Express"
            };
            EcpResponse {
                status: 200,
                headers: Vec::new(),
                body: format!(
                    "<device-info>\n<model-name>{model}</model-name>\
                     \n<model-number>9999X</model-number>\
                     \n<software-version>0.0.0</software-version>\
                     \n<serial-number>FIXTURESERIAL</serial-number>\
                     \n<is-tv>{}</is-tv>\n</device-info>",
                    host == "192.0.2.11"
                ),
            }
        } else {
            EcpResponse {
                status: 404,
                headers: Vec::new(),
                body: String::new(),
            }
        }
    }
}

impl Transport for WireTransport {
    async fn send(&self, host: &str, port: u16, cmd: &Command) -> Result<EcpResponse> {
        Ok(self.send_sync(host, port, cmd))
    }
}

#[test]
fn reconcile_pipeline_produces_devices_from_wire_replies() {
    let transport = WireTransport;
    let ep_a = parse_ssdp_response(REPLY_OK).unwrap();
    let ep_b = parse_ssdp_response(REPLY_TV).unwrap();

    // The same probe+reconcile loop `discover` runs (sync here; the async
    // wrapper is a straight `.await` of this shape).
    let mut devices = Vec::new();
    for ep in [&ep_a, &ep_b] {
        let cmd = zestroku_proto::query("device-info");
        let response = transport.send_sync(&ep.host, ep.port, &cmd);
        assert_eq!(response.status, 200, "device-info on {}", ep.host);
        let info = zestroku_proto::parse_device_info(&response.body).unwrap();
        devices.push(reconcile(ep, &info));
    }
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].model_name, "Roku Express");
    assert_eq!(devices[1].model_name, "Roku TV");
    assert!(devices[1].is_tv);
    assert_eq!(devices[0].access_tier, zestroku_types::AccessTier::Open);
}
