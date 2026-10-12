//! zestroku-tier-discovery-jt8 [d] (AM50018): `devices` must persist every
//! discovered device through the real store. Failing-first: before the fix,
//! `devices` never opens the store, so the DB holds zero device rows.
//!
//! Real sockets end-to-end: two zestroku-sim devices (Express + Tv, both
//! Permissive) on loopback, a targets.toml in a tempdir, the real binary
//! via CARGO_BIN_EXE_zestroku, then the tempfile DB read back through
//! SqliteStore. Privacy is asserted structurally: no row field may contain
//! the sim's FIXTURESERIAL/FIXTUREUDN/FIXTUREDEVICEID placeholder tokens.
#![forbid(unsafe_code)]

use std::process::Command;

use zestroku_core::store::{SqliteStore, Store};
use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

fn write_targets(dir: &std::path::Path, ports: &[u16]) -> std::path::PathBuf {
    let mut text = String::new();
    for p in ports {
        text.push_str(&format!("[[target]]\nhost = \"127.0.0.1\"\nport = {p}\n"));
    }
    let path = dir.join("targets.toml");
    std::fs::write(&path, text).expect("write targets.toml");
    path
}

fn run_devices(db: &std::path::Path, targets: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .args(["devices", "--no-ssdp", "--targets"])
        .arg(targets)
        .output()
        .expect("run zestroku devices");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).expect("stdout UTF-8"),
        String::from_utf8(out.stderr).expect("stderr UTF-8"),
    )
}

#[test]
fn devices_persists_one_row_per_discovered_device() {
    let express = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let tv = SimDevice::start(SimConfig {
        kind: DeviceKind::Tv,
        tier: SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let targets = write_targets(dir.path(), &[express.port(), tv.port()]);

    // First run: discovery + persist.
    let (code, _stdout, stderr) = run_devices(&db, &targets);
    assert_eq!(code, 0, "first run failed: {stderr}");

    let store = SqliteStore::open(&db).expect("open store");
    let rows = store.list_devices().expect("list devices");
    assert_eq!(rows.len(), 2, "expected exactly 2 rows, got: {rows:?}");

    let mut ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    ids.sort();
    let mut expected: Vec<String> = vec![
        format!("127.0.0.1:{}", express.port()),
        format!("127.0.0.1:{}", tv.port()),
    ];
    expected.sort();
    assert_eq!(ids, expected, "ids must be host:port");

    // The Tv row carries the Tv model number.
    let tv_row = rows
        .iter()
        .find(|r| r.model_num == "SIMT1")
        .expect("a Tv row with the SIMT1 model number");
    assert_eq!(tv_row.name, "Sim TV");
    assert!(!tv_row.firmware.is_empty(), "firmware recorded");
    assert_eq!(tv_row.wifi_driver, "", "wifi_driver stays empty");

    // Privacy: no fixture identifier token may appear in any stored field.
    for row in &rows {
        for bad in ["FIXTURESERIAL", "FIXTUREUDN", "FIXTUREDEVICEID"] {
            assert!(
                !row.id.contains(bad)
                    && !row.name.contains(bad)
                    && !row.model.contains(bad)
                    && !row.model_num.contains(bad)
                    && !row.firmware.contains(bad)
                    && !row.wifi_driver.contains(bad),
                "row {row:?} leaked fixture token {bad}"
            );
        }
    }
    drop(store);

    // Second run: idempotent upsert — still exactly 2 rows, same ids.
    let (code, _stdout, stderr) = run_devices(&db, &targets);
    assert_eq!(code, 0, "second run failed: {stderr}");
    let store = SqliteStore::open(&db).expect("reopen store");
    let rows = store.list_devices().expect("list devices after rerun");
    assert_eq!(
        rows.len(),
        2,
        "second run must not duplicate rows: {rows:?}"
    );
    assert!(
        rows.iter()
            .all(|r| r.last_seen >= rows.iter().map(|x| x.last_seen).min().unwrap()),
        "last_seen refreshed"
    );
}
