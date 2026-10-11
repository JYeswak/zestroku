//! `devices --scan <cidr>`: discovery without multicast (macOS gates UDP
//! multicast per executable; TCP to the ECP port is not gated). Real
//! sockets: a zestroku-sim device on loopback, the real binary, no SSDP,
//! no targets file. Planted negatives: a closed port finds nothing, and a
//! public or too-wide range is refused before any connect.
#![forbid(unsafe_code)]

use std::process::Command;

use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

fn run(args: &[&str], db: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .env_remove("ZESTROKU_TARGETS")
        .args(["devices", "--no-ssdp"])
        .args(args)
        .output()
        .expect("run zestroku devices");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).expect("stdout UTF-8"),
        String::from_utf8(out.stderr).expect("stderr UTF-8"),
    )
}

#[test]
fn scan_finds_the_sim_device_on_its_port() {
    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let port = sim.port().to_string();
    let (code, stdout, stderr) = run(
        &["--scan", "127.0.0.1/32", "--scan-port", port.as_str()],
        &db,
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("127.0.0.1:"), "{stdout}");
    assert!(stderr.contains("1 answered"), "{stderr}");
}

#[test]
fn scan_of_a_closed_port_finds_nothing() {
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("addr").port()
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let port = closed.to_string();
    let (code, stdout, stderr) = run(
        &["--scan", "127.0.0.1/32", "--scan-port", port.as_str()],
        &db,
    );
    assert_eq!(code, 1, "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("0 answered"), "{stderr}");
}

#[test]
fn scan_refuses_public_and_too_wide_ranges() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    for cidr in ["8.8.8.0/24", "192.168.0.0/16"] {
        let (code, _, stderr) = run(&["--scan", cidr], &db);
        assert_eq!(code, 2, "{cidr}: {stderr}");
        assert!(stderr.contains("--scan"), "{cidr}: {stderr}");
    }
}
