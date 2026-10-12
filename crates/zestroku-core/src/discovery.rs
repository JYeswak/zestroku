//! Discovery, pure parts: SSDP M-SEARCH build/parse, targets.toml seeds, and
//! reconciliation of a `device-info` probe into a `Device`. No I/O here; the
//! UDP socket and the per-hit probe sit behind `Transport`/the daemon.

use zestroku_proto::DeviceInfo;
use zestroku_types::{AccessTier, Device, Error, Result};

pub const SSDP_ADDR: &str = "239.255.255.250:1900";
pub const SSDP_ST: &str = "roku:ecp";
pub const ECP_DEFAULT_PORT: u16 = 8060;
/// Responses larger than this are refused (MTU-safe bounded read).
pub const MAX_DATAGRAM: usize = 1500;

/// The M-SEARCH request datagram for `roku:ecp`.
pub fn build_msearch(mx_secs: u8) -> Vec<u8> {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDR}\r\nMAN: \"ssdp:discover\"\r\nMX: {mx_secs}\r\nST: {SSDP_ST}\r\n\r\n"
    )
    .into_bytes()
}

/// A reachable ECP endpoint found by SSDP or seeded by targets.toml.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

/// Parse one SSDP response datagram. Refuses oversize datagrams, non-200
/// replies, any ST other than exactly `roku:ecp`, and a missing/odd LOCATION.
pub fn parse_ssdp_response(datagram: &[u8]) -> Result<Endpoint> {
    let bad = |m: &str| Error::Malformed(format!("ssdp: {m}"));
    if datagram.len() > MAX_DATAGRAM {
        return Err(bad("datagram exceeds bounded read"));
    }
    let text = std::str::from_utf8(datagram).map_err(|_| bad("not utf-8"))?;
    let mut lines = text.split("\r\n");
    let status = lines.next().unwrap_or_default();
    if status.split_whitespace().nth(1) != Some("200") {
        return Err(bad("not a 200 response"));
    }
    let (mut st, mut location) = (None, None);
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "st" => st = Some(v.trim()),
                "location" => location = Some(v.trim()),
                _ => {}
            }
        }
    }
    if st != Some(SSDP_ST) {
        return Err(bad("ST is not roku:ecp"));
    }
    let rest = location
        .and_then(|l| l.strip_prefix("http://"))
        .ok_or_else(|| bad("missing http LOCATION"))?;
    let authority = rest.split('/').next().unwrap_or_default();
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| bad("bad LOCATION port"))?),
        None => (authority, ECP_DEFAULT_PORT),
    };
    if host.is_empty() {
        return Err(bad("empty LOCATION host"));
    }
    Ok(Endpoint {
        host: host.to_owned(),
        port,
    })
}

/// Parse `targets.toml`: `[[target]]` tables with `host` and optional `port`.
pub fn parse_targets(toml_text: &str) -> Result<Vec<Endpoint>> {
    let bad = |m: String| Error::Malformed(format!("targets.toml: {m}"));
    let doc: toml::Table = toml_text.parse().map_err(|e| bad(format!("{e}")))?;
    let Some(list) = doc.get("target") else {
        return Ok(Vec::new());
    };
    let arr = list
        .as_array()
        .ok_or_else(|| bad("`target` must be an array of tables".into()))?;
    arr.iter()
        .map(|t| {
            let host = t
                .get("host")
                .and_then(toml::Value::as_str)
                .filter(|h| !h.is_empty())
                .ok_or_else(|| bad("target missing `host`".into()))?;
            let port = match t.get("port") {
                None => ECP_DEFAULT_PORT,
                Some(p) => p
                    .as_integer()
                    .and_then(|n| u16::try_from(n).ok())
                    .ok_or_else(|| bad("bad `port`".into()))?,
            };
            Ok(Endpoint {
                host: host.to_owned(),
                port,
            })
        })
        .collect()
}

/// Union of SSDP hits and seeds, de-duplicated, stable order. Seeds alone
/// (empty `found`) still yield every seeded endpoint.
pub fn merge_endpoints(found: &[Endpoint], seeds: &[Endpoint]) -> Vec<Endpoint> {
    let mut all: Vec<Endpoint> = found.iter().chain(seeds).cloned().collect();
    all.sort();
    all.dedup();
    all
}

/// Widest scan the CLI will run: a /22 (1022 hosts). A bigger sweep is not
/// a home-LAN sweep and is refused.
pub const SCAN_MAX_PREFIX_WIDTH: u8 = 22;

/// Expand `a.b.c.d/n` into the host addresses to probe (non-multicast
/// discovery: a bounded TCP probe of ECP's port). Own-LAN rule: only
/// RFC 1918 and loopback ranges, and at most a /22. For prefixes shorter
/// than /31 the network and broadcast addresses are excluded.
pub fn expand_cidr(cidr: &str) -> Result<Vec<std::net::Ipv4Addr>> {
    let bad = |m: &str| Error::Malformed(format!("--scan {cidr:?}: {m}"));
    let (addr, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| bad("expected a.b.c.d/n"))?;
    let addr: std::net::Ipv4Addr = addr.parse().map_err(|_| bad("not an IPv4 address"))?;
    let prefix: u8 = prefix.parse().map_err(|_| bad("bad prefix length"))?;
    if prefix > 32 {
        return Err(bad("prefix longer than /32"));
    }
    if prefix < SCAN_MAX_PREFIX_WIDTH {
        return Err(bad("wider than /22 (1022 hosts) is not a home-LAN sweep"));
    }
    if !(addr.is_private() || addr.is_loopback()) {
        return Err(bad(
            "only private (RFC 1918) or loopback ranges are scanned",
        ));
    }
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    let base = u32::from(addr) & mask;
    let last = base | !mask;
    let (first, end) = match prefix {
        32 => (base, base),
        31 => (base, last),
        _ => (base + 1, last - 1),
    };
    Ok((first..=end).map(std::net::Ipv4Addr::from).collect())
}

/// Stable identity key for one physical device, derived from its
/// serial-number/UDN identity fields. The output is a hex digest: raw
/// serials never appear in CLI output, logs, fixtures or the alias table,
/// so FIXTURESERIAL-class scrub gates keep passing. Empty identity fields
/// yield empty identity (unknown: never merged, fail-safe for old rows).
pub fn identity_key(info: &DeviceInfo) -> String {
    if info.serial_number.is_empty() && info.udn.is_empty() {
        return String::new();
    }
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"roku-device-identity/v1\0");
    hasher.update(info.serial_number.as_bytes());
    hasher.update(b"\0");
    hasher.update(info.udn.as_bytes());
    crate::mutation::hex(hasher.finalize().as_ref())
}

/// Reconcile an endpoint plus its `query/device-info` into a `Device`.
/// Tier starts `Open` (the safe assumption) until a probe says otherwise.
pub fn reconcile(ep: &Endpoint, info: &DeviceInfo) -> Device {
    Device {
        host: ep.host.clone(),
        port: ep.port,
        model_name: info.model_name.clone(),
        model_number: info.model_number.clone(),
        software_version: info.software_version.clone(),
        is_tv: info.is_tv,
        access_tier: AccessTier::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = "HTTP/1.1 200 OK\r\nCache-Control: max-age=3600\r\nST: roku:ecp\r\nUSN: uuid:roku:ecp:TEST\r\nLocation: http://192.0.2.10:8060/\r\n\r\n";

    fn ep(h: &str) -> Endpoint {
        Endpoint {
            host: h.into(),
            port: 8060,
        }
    }

    #[test]
    fn msearch_is_exact() {
        let s = String::from_utf8(build_msearch(3)).unwrap();
        assert!(s.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(s.contains("ST: roku:ecp\r\n"));
        assert!(s.contains("MAN: \"ssdp:discover\"\r\n"));
        assert!(s.ends_with("\r\n\r\n"));
    }

    #[test]
    fn parses_roku_response() {
        assert_eq!(
            parse_ssdp_response(OK.as_bytes()).unwrap(),
            ep("192.0.2.10")
        );
    }

    #[test]
    fn refuses_non_roku_st() {
        // Planted negative: each of these must be refused; removing the ST check turns the first red.
        let dial = OK.replace(
            "roku:ecp\r\nUSN",
            "urn:dial-multiscreen-org:service:dial:1\r\nUSN",
        );
        assert!(parse_ssdp_response(dial.as_bytes()).is_err());
        let rtv = OK.replace("ST: roku:ecp", "ST: roku:rtvaccel-123");
        assert!(parse_ssdp_response(rtv.as_bytes()).is_err());
        let none = OK.replace("ST: roku:ecp\r\n", "");
        assert!(parse_ssdp_response(none.as_bytes()).is_err());
    }

    #[test]
    fn refuses_oversize_non200_and_bad_location() {
        let big = format!("{OK}{}", "x".repeat(MAX_DATAGRAM));
        assert!(parse_ssdp_response(big.as_bytes()).is_err());
        assert!(parse_ssdp_response(OK.replace("200 OK", "404 Not Found").as_bytes()).is_err());
        assert!(parse_ssdp_response(OK.replace("http://", "ftp://").as_bytes()).is_err());
        assert!(parse_ssdp_response(OK.replace(":8060", ":99999").as_bytes()).is_err());
    }

    #[test]
    fn location_without_port_defaults() {
        let r = OK.replace(":8060/", "/");
        assert_eq!(parse_ssdp_response(r.as_bytes()).unwrap().port, 8060);
    }

    #[test]
    fn targets_seed_only_run_inventories_all() {
        let seeds = parse_targets(
            "[[target]]\nhost = \"192.0.2.10\"\n[[target]]\nhost = \"192.0.2.11\"\nport = 8061\n",
        )
        .unwrap();
        assert_eq!(seeds.len(), 2);
        assert_eq!(seeds[1].port, 8061);
        // multicast black-holed: no SSDP hits, seeds still produce both endpoints
        assert_eq!(merge_endpoints(&[], &seeds).len(), 2);
        // a moved device found by SSDP joins the seeds without duplicating
        let moved = [ep("192.0.2.99"), ep("192.0.2.10")];
        assert_eq!(merge_endpoints(&moved, &seeds).len(), 3);
    }

    #[test]
    fn targets_rejects_bad_input() {
        assert!(parse_targets("[[target]]\nport = 8060\n").is_err());
        assert!(parse_targets("[[target]]\nhost = \"h\"\nport = 70000\n").is_err());
        assert!(parse_targets("not toml [[").is_err());
        assert!(parse_targets("").unwrap().is_empty());
    }

    #[test]
    fn reconcile_maps_device_info() {
        let info = DeviceInfo {
            model_name: "Roku Express 4K+".into(),
            model_number: "3941X".into(),
            software_version: "15.0.0".into(),
            is_tv: false,
            serial_number: "FIXTURESERIAL".into(),
            udn: "FIXTUREUDN".into(),
        };
        let d = reconcile(&ep("192.0.2.10"), &info);
        assert_eq!(
            (d.host.as_str(), d.port, d.is_tv),
            ("192.0.2.10", 8060, false)
        );
        assert_eq!(d.model_number, "3941X");
    }

    fn identified(serial: &str, udn: &str) -> DeviceInfo {
        DeviceInfo {
            model_name: "Sim".into(),
            model_number: "SIMX".into(),
            software_version: "1".into(),
            is_tv: false,
            serial_number: serial.into(),
            udn: udn.into(),
        }
    }

    #[test]
    fn identity_key_is_stable_and_hex() {
        let a = identity_key(&identified("FIXTURESERIAL-A", "FIXTUREUDN-A"));
        assert_eq!(
            a,
            identity_key(&identified("FIXTURESERIAL-A", "FIXTUREUDN-A"))
        );
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(
            !a.contains("FIXTURE"),
            "raw serial must not leak into the key"
        );
    }

    #[test]
    fn identity_key_separates_devices_and_is_empty_when_unknown() {
        // Planted negative: same model, different serials must not merge.
        // Dropping the serial from the digest turns this red.
        assert_ne!(
            identity_key(&identified("FIXTURESERIAL-A", "U")),
            identity_key(&identified("FIXTURESERIAL-B", "U"))
        );
        assert_eq!(identity_key(&identified("", "")), "");
    }

    #[test]
    fn expand_cidr_excludes_network_and_broadcast() {
        let hosts = expand_cidr("192.168.1.77/24").unwrap();
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts[0].to_string(), "192.168.1.1");
        assert_eq!(hosts[253].to_string(), "192.168.1.254");
    }

    #[test]
    fn expand_cidr_small_prefixes() {
        assert_eq!(expand_cidr("192.168.1.9/32").unwrap().len(), 1);
        assert_eq!(expand_cidr("192.168.1.8/31").unwrap().len(), 2);
        assert_eq!(
            expand_cidr("127.0.0.1/32").unwrap()[0].to_string(),
            "127.0.0.1"
        );
        assert_eq!(expand_cidr("10.0.0.0/22").unwrap().len(), 1022);
    }

    #[test]
    fn expand_cidr_refuses_public_wide_and_malformed() {
        // Planted negatives: each removal of a guard turns one of these red.
        assert!(expand_cidr("8.8.8.0/24").is_err(), "public range");
        assert!(expand_cidr("192.168.0.0/16").is_err(), "wider than /22");
        assert!(expand_cidr("192.168.1.0/33").is_err(), "prefix > 32");
        assert!(expand_cidr("192.168.1.0").is_err(), "no prefix");
        assert!(expand_cidr("not-an-ip/24").is_err(), "not an address");
        assert!(expand_cidr("192.168.1.0/x").is_err(), "bad prefix");
    }
}
