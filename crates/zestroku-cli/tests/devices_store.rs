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
    // Distinct serial placeholders: same FIXTURE class, different identities,
    // so the two devices must never merge into one row (see the no-merge
    // coverage below and identity_key_separates_devices in core).
    let express = SimDevice::start_with_serial(
        SimConfig {
            kind: DeviceKind::Express,
            tier: SimTier::Permissive,
        },
        "FIXTURESERIAL-A",
    );
    let tv = SimDevice::start_with_serial(
        SimConfig {
            kind: DeviceKind::Tv,
            tier: SimTier::Permissive,
        },
        "FIXTURESERIAL-B",
    );
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

fn run_alias(db: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zestroku"));
    cmd.env("ZESTROKU_DB", db).arg("alias");
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("run zestroku alias");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).expect("stdout UTF-8"),
        String::from_utf8(out.stderr).expect("stderr UTF-8"),
    )
}

#[test]
fn moved_device_keeps_one_row_and_aliases_follow() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");

    // Same physical device (same serial placeholder) at its old endpoint.
    let first = SimDevice::start_with_serial(
        SimConfig {
            kind: DeviceKind::Express,
            tier: SimTier::Permissive,
        },
        "FIXTURESERIAL-MOVE",
    );
    let old_id = format!("127.0.0.1:{}", first.port());
    let targets = write_targets(dir.path(), &[first.port()]);
    let (code, _stdout, stderr) = run_devices(&db, &targets);
    assert_eq!(code, 0, "first run failed: {stderr}");
    let (code, _stdout, stderr) = run_alias(&db, &["set", "living-room", &old_id]);
    assert_eq!(code, 0, "alias set failed: {stderr}");

    // DHCP move: the same serial reappears at a new endpoint.
    drop(first);
    let second = SimDevice::start_with_serial(
        SimConfig {
            kind: DeviceKind::Express,
            tier: SimTier::Permissive,
        },
        "FIXTURESERIAL-MOVE",
    );
    let new_id = format!("127.0.0.1:{}", second.port());
    assert_ne!(old_id, new_id, "move needs a new endpoint");
    let targets = write_targets(dir.path(), &[second.port()]);
    let (code, _stdout, stderr) = run_devices(&db, &targets);
    assert_eq!(code, 0, "rerun after move failed: {stderr}");

    let store = SqliteStore::open(&db).expect("open store");
    let rows = store.list_devices().expect("list devices");
    assert_eq!(rows.len(), 1, "move must not duplicate rows: {rows:?}");
    assert_eq!(rows[0].id, new_id, "row follows the new endpoint");
    let aliases = store.list_aliases().expect("list aliases");
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].alias, "living-room");
    assert_eq!(
        aliases[0].device_id, new_id,
        "alias follows the moved device"
    );
    drop(second);
}

#[test]
fn same_model_different_identities_never_merge() {
    // Planted negative: two same-model sims with different serials must stay
    // two rows. Merging on model (or dropping identity from the lookup)
    // turns this red.
    let first = SimDevice::start_with_serial(
        SimConfig {
            kind: DeviceKind::Express,
            tier: SimTier::Permissive,
        },
        "FIXTURESERIAL-NOMERGE-A",
    );
    let second = SimDevice::start_with_serial(
        SimConfig {
            kind: DeviceKind::Express,
            tier: SimTier::Permissive,
        },
        "FIXTURESERIAL-NOMERGE-B",
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let targets = write_targets(dir.path(), &[first.port(), second.port()]);
    let (code, _stdout, stderr) = run_devices(&db, &targets);
    assert_eq!(code, 0, "run failed: {stderr}");
    let store = SqliteStore::open(&db).expect("open store");
    let rows = store.list_devices().expect("list devices");
    assert_eq!(
        rows.len(),
        2,
        "distinct identities must not merge: {rows:?}"
    );
    drop(first);
    drop(second);
}

#[test]
fn unknown_identity_same_model_devices_never_merge() {
    // Planted negative: neither sim advertises a serial or UDN. Empty
    // identity must not collapse distinct same-model endpoints in the store.
    let config = SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    };
    let first = SimDevice::start_with_serial(config, "");
    first.set_unknown_identity();
    let second = SimDevice::start_with_serial(config, "");
    second.set_unknown_identity();

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let targets = write_targets(dir.path(), &[first.port(), second.port()]);
    let (code, _stdout, stderr) = run_devices(&db, &targets);
    assert_eq!(code, 0, "run failed: {stderr}");

    let store = SqliteStore::open(&db).expect("open store");
    let rows = store.list_devices().expect("list devices");
    assert_eq!(
        rows.len(),
        2,
        "empty identity must not merge same-model devices: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| row.model_num == "SIMX1"),
        "the negative must use two same-model devices: {rows:?}"
    );
    for sim in [&first, &second] {
        assert_eq!(
            sim.received_requests(),
            vec![("GET".into(), "/query/device-info".into())],
            "each endpoint is queried once"
        );
    }
}

#[test]
fn cli_output_never_emits_fixture_identifiers() {
    // Acceptance [c]: every CLI/JSON/log surface must be free of the sim's
    // FIXTURESERIAL-class tokens. The identity key is a digest by
    // construction; this test pins the observable surfaces.
    let sim = SimDevice::start_with_serial(
        SimConfig {
            kind: DeviceKind::Express,
            tier: SimTier::Permissive,
        },
        "FIXTURESERIAL-SCRUB",
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let targets = write_targets(dir.path(), &[sim.port()]);
    let (code, stdout, stderr) = run_devices(&db, &targets);
    assert_eq!(code, 0, "run failed: {stderr}");
    let json_out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .args(["devices", "--no-ssdp", "--targets"])
        .arg(&targets)
        .arg("--json")
        .output()
        .expect("run zestroku devices --json");
    let json_stdout = String::from_utf8(json_out.stdout).expect("json UTF-8");
    assert_eq!(json_out.status.code(), Some(0));
    for surface in [&stdout, &stderr, &json_stdout] {
        for bad in ["FIXTURESERIAL", "FIXTUREUDN", "FIXTUREDEVICEID"] {
            assert!(
                !surface.contains(bad),
                "CLI surface leaked fixture token {bad}"
            );
        }
    }
    drop(sim);
}
