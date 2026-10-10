//! Repro for an upstream asupersync defect: on macOS, `TcpStream::connect` to this
//! machine's own non-loopback (LAN) address fails synchronously, while
//! `std::net::TcpStream::connect` to the same address succeeds.
//! No external host needed: the listener is local.
use std::net::{IpAddr, TcpListener, TcpStream as StdStream, UdpSocket};
use std::time::Instant;

use asupersync::net::TcpStream;
use asupersync::runtime::{reactor::create_reactor, RuntimeBuilder};

/// Primary LAN address. UDP connect only selects a route; no packet is sent.
fn lan_ip() -> IpAddr {
    let s = UdpSocket::bind("0.0.0.0:0").expect("udp bind");
    s.connect("192.0.2.1:9").expect("udp route");
    s.local_addr().expect("local addr").ip()
}

fn main() {
    let listener = TcpListener::bind("0.0.0.0:0").expect("listen");
    let port = listener.local_addr().expect("addr").port();
    let lan = lan_ip();
    println!("listener 0.0.0.0:{port}  LAN address {lan}");

    for host in ["127.0.0.1".to_string(), lan.to_string()] {
        let addr = format!("{host}:{port}");

        let t0 = Instant::now();
        match StdStream::connect(&addr) {
            Ok(_) => println!("std::net    {addr}: ok   in {:?}", t0.elapsed()),
            Err(e) => println!("std::net    {addr}: FAIL {e} in {:?}", t0.elapsed()),
        }

        let rt = RuntimeBuilder::current_thread()
            .with_reactor(create_reactor().expect("reactor"))
            .build()
            .expect("runtime");
        let t0 = Instant::now();
        match rt.block_on(async { TcpStream::connect(addr.clone()).await }) {
            Ok(_) => println!("asupersync  {addr}: ok   in {:?}", t0.elapsed()),
            Err(e) => println!("asupersync  {addr}: FAIL {e} in {:?}", t0.elapsed()),
        }
    }
}
