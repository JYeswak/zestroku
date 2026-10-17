//! `zestroku db backup|restore` as the data half of upgrade and rollback
//! (1r4.9). The real binary against real fsqlite files; no mocks. Device
//! identifiers are synthetic placeholders, never real serials/UDNs/MACs.
#![forbid(unsafe_code)]

use std::path::Path;
use std::process::Command;

use zestroku_core::store::{DeviceRow, MIGRATIONS, SqliteStore, Store};

fn device(id: &str) -> DeviceRow {
    DeviceRow {
        id: id.into(),
        name: "Test Roku".into(),
        ip: "192.0.2.10".into(),
        model: "Express 4K+".into(),
        model_num: "3941X".into(),
        firmware: "15.0.0".into(),
        wifi_driver: "drv".into(),
        access_tier: "permissive".into(),
        last_seen: 1_700_000_000,
    }
}

fn zestroku(db: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .args(args)
        .output()
        .expect("run zestroku");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).expect("stdout UTF-8"),
        String::from_utf8(out.stderr).expect("stderr UTF-8"),
    )
}

fn device_ids(store: &SqliteStore) -> Vec<String> {
    let mut ids: Vec<String> = store
        .list_devices()
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    ids.sort();
    ids
}

/// The catalog an OLDER binary knows: the first two migrations.
fn old_catalog() -> &'static [zestroku_core::store::Migration] {
    assert!(MIGRATIONS.len() > 2, "an older catalog must exist");
    &MIGRATIONS[..2]
}

#[test]
fn upgrade_failure_rolls_back_to_the_pre_upgrade_backup() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let backup = dir.path().join("pre-upgrade.db");
    let backup_arg = backup.to_str().unwrap();

    // Old-version data: an older binary's store at the old schema.
    let old = SqliteStore::open_with(&live, old_catalog()).unwrap();
    old.upsert_device(&device("dev-1")).unwrap();
    old.close().unwrap();

    // The pre-upgrade backup is taken by the NEW binary before any open
    // migrates the data: it must keep the OLD schema it found.
    let (code, stdout, stderr) = zestroku(&live, &["db", "backup", backup_arg]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let snap = SqliteStore::open_with(&backup, old_catalog()).unwrap();
    assert_eq!(
        snap.schema_versions().unwrap(),
        [1, 2],
        "backup not migrated"
    );
    assert_eq!(device_ids(&snap), ["dev-1"]);
    snap.close().unwrap();

    // Upgrade: the new binary opens the live store and migrates it.
    let (code, _, stderr) = zestroku(&live, &["alias", "ls"]);
    assert_eq!(code, 0, "{stderr}");
    let migrated = SqliteStore::open(&live).unwrap();
    assert_eq!(
        migrated.schema_versions().unwrap().len(),
        MIGRATIONS.len(),
        "the upgrade migrated the live store"
    );
    migrated.close().unwrap();

    // Newer-schema data: the older catalog refuses to open it (1r4.7).
    assert!(
        SqliteStore::open_with(&live, old_catalog()).is_err(),
        "an older binary must refuse newer-schema data"
    );

    // The candidate failed: validated compatible rollback of the data.
    let (code, stdout, stderr) = zestroku(&live, &["db", "restore", backup_arg]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");

    // The older binary reads the restored data again, at the old schema.
    let back = SqliteStore::open_with(&live, old_catalog()).unwrap();
    assert_eq!(back.schema_versions().unwrap(), [1, 2]);
    assert_eq!(device_ids(&back), ["dev-1"]);
    back.close().unwrap();
}

#[test]
fn a_migrating_open_is_not_a_pre_upgrade_backup() {
    // Causal control for the test above: a snapshot taken from an OPENED
    // store has already been migrated, so rollback to it would hand the old
    // binary newer-schema data. `db backup` exists to avoid exactly this.
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let old = SqliteStore::open_with(&live, old_catalog()).unwrap();
    old.upsert_device(&device("dev-1")).unwrap();
    old.close().unwrap();

    let opened = SqliteStore::open(&live).unwrap();
    let migrated_snapshot = dir.path().join("migrated.db");
    opened.backup_to(&migrated_snapshot).unwrap();
    opened.close().unwrap();

    assert!(
        SqliteStore::open_with(&migrated_snapshot, old_catalog()).is_err(),
        "a backup of an already-migrated store is newer-schema data"
    );
}

#[test]
fn backup_refuses_to_overwrite_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    store.upsert_device(&device("dev-1")).unwrap();
    store.close().unwrap();
    let existing = dir.path().join("existing.bak");
    std::fs::write(&existing, b"precious").unwrap();

    let (code, _, _) = zestroku(&live, &["db", "backup", existing.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert_eq!(std::fs::read(&existing).unwrap(), b"precious");
}

#[test]
fn restore_of_a_non_database_exits_nonzero_and_keeps_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    store.upsert_device(&device("dev-1")).unwrap();
    store.close().unwrap();
    let junk = dir.path().join("junk.bak");
    std::fs::write(&junk, b"not a database").unwrap();

    let (code, _, _) = zestroku(&live, &["db", "restore", junk.to_str().unwrap()]);
    assert_eq!(code, 1);
    let kept = SqliteStore::open(&live).unwrap();
    assert_eq!(device_ids(&kept), ["dev-1"]);
    kept.close().unwrap();
}

#[test]
fn unknown_db_verbs_are_usage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let (code, _, _) = zestroku(&live, &["db", "frobnicate"]);
    assert_eq!(code, 2);
    let (code, _, _) = zestroku(&live, &["db", "backup"]);
    assert_eq!(code, 2);
    assert!(!live.exists(), "usage errors never create the store");
}
