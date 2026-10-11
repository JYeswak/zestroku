//! Tier-gate regression core (m7e extension + zestroku-tier-discovery-jt8):
//! POST verbs are refused locally before dispatch, and the refusal is
//! audited. Since tier-discovery, the CLI trusts the RECORDED tier: an
//! unprobed host refuses with "no recorded tier" (exit 3, zero dispatches),
//! and a recorded-Limited host refuses at the tier wall (also exit 3,
//! TIER_REFUSED audit row). Encodes the wire-doc finding: the gate is a
//! wrapper, not a filter; local refusal means no 403 round-trip.
use std::process::Command;

use zestroku_core::store::{AccessTierRow, DeviceRow, SqliteStore, Store};

fn run(args: &[&str], db: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .args(args)
        .output()
        .expect("run zestroku command");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).expect("stdout UTF-8"),
        String::from_utf8(out.stderr).expect("stderr UTF-8"),
    )
}

/// Seeds a device whose recorded tier is `limited` (the m3-flipped reality
/// is permissive, but the tier WALL is what these tests pin: a recorded
/// Open/Limited tier refuses POSTs at the check_tier wall either way).
fn seed_limited(db: &std::path::Path) {
    let store = SqliteStore::open(db).expect("seed store open");
    store
        .upsert_device(&DeviceRow {
            id: "127.0.0.1".into(),
            name: "seeded".into(),
            ip: "127.0.0.1".into(),
            model: "Sim Express".into(),
            model_num: "SIMX1".into(),
            firmware: "15.2.4".into(),
            wifi_driver: "wifi".into(),
            access_tier: "limited".into(),
            last_seen: 1,
        })
        .expect("seed device");
    store
        .upsert_access_tier(&AccessTierRow {
            device_id: "127.0.0.1".into(),
            tier: "limited".into(),
            probe_at: 1,
            probe_map: r#"{"query/apps":"limited403"}"#.into(),
        })
        .expect("seed tier");
}

#[test]
fn keypress_is_refused_before_network_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("a.db");
    seed_limited(&db);
    let (code, _stdout, stderr) = run(&["key", "127.0.0.1", "Home"], &db);
    assert_eq!(code, 3);
    assert!(stderr.contains("not allowed in Open mode"), "{stderr}");
}

#[test]
fn launch_is_refused_for_recorded_limited_tier_even_with_confirm() {
    // The tier gate runs BEFORE confirmation: a token cannot buy a mutation
    // past the tier wall. This pins ordering regression m7e guards.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("a.db");
    seed_limited(&db);
    let (code, stdout, stderr) = run(&["launch", "127.0.0.1", "12", "--confirm", "yes"], &db);
    assert_eq!(code, 3, "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("not allowed in Open mode"), "{stderr}");
    assert!(stderr.contains("denial audited"), "{stderr}");
    assert!(stdout.is_empty(), "nothing dispatched: {stdout}");
    // The refusal is audited: one TIER_REFUSED row exists.
    let store = SqliteStore::open(&db).expect("reopen");
    let rows = store.list_actions().expect("list actions");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].result, "TIER_REFUSED");
}

#[test]
fn unprobed_host_refuses_without_reaching_any_gate() {
    // zestroku-tier-discovery-jt8: an unprobed host is refused BEFORE the
    // gate — exit 3, message names `zestroku info`, zero audit rows.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("a.db");
    let (code, _stdout, stderr) = run(&["launch", "127.0.0.1", "12"], &db);
    assert_eq!(code, 3);
    assert!(stderr.contains("no recorded tier"), "{stderr}");
    assert!(stderr.contains("zestroku info"), "{stderr}");
    assert!(!stderr.contains("CONFIRMATION_REQUIRED"), "{stderr}");
    let store = SqliteStore::open(&db).expect("reopen");
    assert!(store.list_actions().expect("list").is_empty());
}

#[test]
fn get_commands_still_dispatch_without_any_gate() {
    // Sanity anchor: reads remain Open-tier and ungated. 127.0.0.1:8060 has
    // nothing listening -> the CLI reports an unreachable device (exit 1),
    // NOT a tier refusal and NOT a confirmation refusal.
    let dir = tempfile::tempdir().unwrap();
    let (code, _stdout, stderr) = run(&["active-app", "127.0.0.1"], &dir.path().join("a.db"));
    assert_ne!(code, 3, "reads must not hit the tier wall: {stderr}");
    assert_ne!(code, 4, "reads must not hit the confirmation gate: {stderr}");
}
