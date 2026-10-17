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
        .env("ZESTROKU_DIRECT", "1")
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
    assert!(stdout.is_empty(), "closed ports must not produce a device");
}

#[test]
fn scan_refuses_public_and_too_wide_ranges() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    for cidr in ["8.8.8.0/24", "192.168.0.0/16"] {
        let (code, _, stderr) = run(&["--scan", cidr], &db);
        assert_eq!(code, 2, "{cidr}: {stderr}");
    }
}

#[test]
fn explicit_direct_apps_lists_sim_installed_apps() {
    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let target = format!("127.0.0.1:{}", sim.port());
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .env("ZESTROKU_TIER", "permissive")
        .env("ZESTROKU_DIRECT", "1")
        .args(["apps", target.as_str()])
        .output()
        .expect("run zestroku apps");
    let stdout = String::from_utf8(out.stdout).expect("stdout UTF-8");
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains("Streaming One") && stdout.contains("1001"),
        "{stdout}"
    );
}

#[test]
fn scan_tolerates_suspended_sim_then_finds_it_after_resume() {
    let mut sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let port = sim.port().to_string();
    sim.suspend();
    let (code, _, stderr) = run(
        &["--scan", "127.0.0.1/32", "--scan-port", port.as_str()],
        &db,
    );
    assert_eq!(code, 1, "{stderr}");
    sim.resume();
    let (code, stdout, stderr) = run(
        &["--scan", "127.0.0.1/32", "--scan-port", port.as_str()],
        &db,
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("127.0.0.1:"), "{stdout}");
}

#[test]
fn discovery_uses_selected_transport_and_warns_for_unknown_selector() {
    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let targets = dir.path().join("targets.toml");
    std::fs::write(
        &targets,
        format!("[[target]]\nhost = \"127.0.0.1\"\nport = {}\n", sim.port()),
    )
    .expect("write targets");
    let targets = targets.to_str().expect("targets path UTF-8");

    for (selector, warns) in [("std", false), ("bogus", true)] {
        let before = sim.received_requests().len();
        let output = Command::new(env!("CARGO_BIN_EXE_zestroku"))
            .env("ZESTROKU_DB", &db)
            .env("ZESTROKU_DIRECT", "1")
            .env("ZESTROKU_TRANSPORT", selector)
            .env_remove("ZESTROKU_TARGETS")
            .args(["devices", "--targets", targets, "--mx", "1", "--json"])
            .output()
            .expect("run zestroku devices");
        let stdout = String::from_utf8(output.stdout).expect("stdout UTF-8");
        let stderr = String::from_utf8(output.stderr).expect("stderr UTF-8");
        assert_eq!(
            output.status.code(),
            Some(0),
            "selector={selector} stdout={stdout} stderr={stderr}"
        );
        let report: serde_json::Value = serde_json::from_str(&stdout).expect("devices JSON");
        assert!(
            report["devices"].as_array().is_some_and(|devices| {
                devices.iter().any(|device| {
                    device["host"] == "127.0.0.1"
                        && device["port"].as_u64() == Some(u64::from(sim.port()))
                })
            }),
            "selector={selector}: {report}"
        );
        assert_eq!(
            stderr.contains("unknown ZESTROKU_TRANSPORT"),
            warns,
            "selector={selector} stderr={stderr}"
        );
        assert!(
            sim.received_requests()[before..]
                .iter()
                .any(|(method, path)| method == "GET" && path == "/query/device-info"),
            "selector={selector} did not probe the seeded simulator"
        );
    }
}
