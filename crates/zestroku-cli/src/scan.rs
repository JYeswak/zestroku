//! Non-multicast discovery: probe a CIDR for an open ECP port.
//!
//! macOS gates UDP multicast per executable (measured 2026-10-11: one file
//! path found both Rokus via SSDP, byte-identical copies found none, while
//! TCP :8060 worked from every copy). A bounded TCP connect sweep needs no
//! grant. Open hosts become `[[target]]` seeds that the normal
//! `device-info` probe confirms, so a non-Roku listener on the port is
//! dropped there, not trusted here.

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Per-host connect budget. A LAN host that is up answers in milliseconds;
/// a dead address costs at most this.
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);

/// Concurrent connect attempts (blocking std threads).
const WORKERS: usize = 64;

/// Hosts from `hosts` that accept a TCP connection on `port`, ascending.
pub fn open_hosts(hosts: &[Ipv4Addr], port: u16, timeout: Duration) -> Vec<Ipv4Addr> {
    let next = AtomicUsize::new(0);
    let found = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(hosts.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(host) = hosts.get(i) else { break };
                    let addr = SocketAddr::from((*host, port));
                    if TcpStream::connect_timeout(&addr, timeout).is_ok() {
                        found.lock().expect("found lock").push(*host);
                    }
                }
            });
        }
    });
    let mut found = found.into_inner().expect("found lock");
    found.sort();
    found
}

/// `targets.toml` text for the open hosts, in the format `parse_targets` reads.
pub fn seeds_text(open: &[Ipv4Addr], port: u16) -> String {
    open.iter()
        .map(|h| format!("[[target]]\nhost = \"{h}\"\nport = {port}\n"))
        .collect()
}

/// The /24 around this host's private IPv4 address, for the no-argument
/// fallback sweep. `connect` on a UDP socket only selects a route (nothing
/// is sent), so this names the interface address the OS would use toward
/// the LAN. `None` when there is no private IPv4 route.
pub fn local_cidr() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) if v4.is_private() => {
            let o = v4.octets();
            Some(format!("{}.{}.{}.0/24", o[0], o[1], o[2]))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn local_cidr_is_a_scannable_private_range_when_present() {
        if let Some(cidr) = local_cidr() {
            let hosts = zestroku_core::discovery::expand_cidr(&cidr).expect("scannable");
            assert_eq!(hosts.len(), 254, "{cidr}");
        }
    }

    #[test]
    fn finds_only_the_listening_host() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hosts = [Ipv4Addr::new(127, 0, 0, 1), Ipv4Addr::new(127, 0, 0, 2)];
        // 127.0.0.2 is loopback too but nothing listens there on `port`
        // (the listener is bound to 127.0.0.1 only).
        let open = open_hosts(&hosts, port, Duration::from_millis(300));
        assert_eq!(open, vec![Ipv4Addr::new(127, 0, 0, 1)]);
    }

    #[test]
    fn closed_port_yields_nothing() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let open = open_hosts(&[Ipv4Addr::LOCALHOST], port, Duration::from_millis(300));
        assert!(open.is_empty(), "{open:?}");
    }

    #[test]
    fn seeds_round_trip_through_parse_targets() {
        let text = seeds_text(&[Ipv4Addr::new(192, 168, 1, 5)], 8060);
        let eps = zestroku_core::discovery::parse_targets(&text).unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!((eps[0].host.as_str(), eps[0].port), ("192.168.1.5", 8060));
    }
}
