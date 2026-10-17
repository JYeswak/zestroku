//! Backup/restore for upgrade rollback and recovery (1r4.9).
//!
//! Real fsqlite temp-file databases; no mocks. Device identifiers below are
//! synthetic placeholders, never real serials/UDNs/MACs.

use fsqlite::{AsyncConnection, SqliteValue};
use zestroku_core::store::{ActionRow, AliasRow, DeviceRow, SqliteStore, Store};

fn device() -> DeviceRow {
    DeviceRow {
        id: "dev-1".into(),
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

fn action() -> ActionRow {
    ActionRow {
        id: 0,
        at: 1_700_000_100,
        client: "cli".into(),
        surface: "keypress".into(),
        intent: "Home".into(),
        decision: "approved".into(),
        result: "delivered".into(),
        before_state: "{\"app\":\"home\"}".into(),
    }
}

fn seed(store: &SqliteStore) {
    store.upsert_device(&device()).unwrap();
    store
        .set_alias(&AliasRow {
            alias: "tv".into(),
            device_id: "dev-1".into(),
        })
        .unwrap();
    store.insert_action(&action()).unwrap();
}

#[test]
fn backup_preserves_inventory_aliases_and_history() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store);
    let dest = dir.path().join("actions.backup.db");
    store.backup_to(&dest).unwrap();
    store.close().unwrap();

    // The snapshot is a standalone database: no WAL sidecars travel with it.
    assert!(!dir.path().join("actions.backup.db-wal").exists());
    assert!(!dir.path().join("actions.backup.db-shm").exists());

    let snap = SqliteStore::open(&dest).unwrap();
    assert_eq!(snap.list_devices().unwrap().len(), 1);
    assert_eq!(snap.list_aliases().unwrap().len(), 1);
    assert_eq!(snap.list_actions().unwrap().len(), 1);
    snap.close().unwrap();
}

#[test]
fn restore_rolls_live_back_to_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store);
    let backup = dir.path().join("actions.backup.db");
    store.backup_to(&backup).unwrap();
    store.close().unwrap();

    // Post-backup drift: a new device lands in live.
    let drifted = SqliteStore::open(&live).unwrap();
    let mut extra = device();
    extra.id = "dev-2".into();
    drifted.upsert_device(&extra).unwrap();
    drifted.close().unwrap();

    SqliteStore::restore_from(&backup, &live).unwrap();

    // No staging file leaks, and live is back to the one-device snapshot.
    assert!(!dir.path().join(".restore.tmp").exists());
    let rolled = SqliteStore::open(&live).unwrap();
    assert_eq!(rolled.list_devices().unwrap().len(), 1);
    rolled.close().unwrap();
}

#[test]
fn restore_refuses_a_newer_schema_backup_and_keeps_live() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store);
    let backup = dir.path().join("actions.backup.db");
    store.backup_to(&backup).unwrap();
    store.close().unwrap();

    // Forge a newer-schema backup: ledger max one past supported.
    let conn = AsyncConnection::open_sync(backup.to_string_lossy().into_owned()).unwrap();
    let max: i64 = zestroku_core::store::MIGRATIONS
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap();
    conn.execute_with_params_sync(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
        &[
            SqliteValue::from(max + 1),
            SqliteValue::from("forged-future"),
            SqliteValue::from(1_700_000_200i64),
        ],
    )
    .unwrap();
    drop(conn);

    let err = SqliteStore::restore_from(&backup, &live).unwrap_err();
    assert!(err.to_string().contains("newer"), "{err}");

    // Live is untouched and still opens with its single device.
    assert!(!dir.path().join(".restore.tmp").exists());
    let kept = SqliteStore::open(&live).unwrap();
    assert_eq!(kept.list_devices().unwrap().len(), 1);
    kept.close().unwrap();
}

#[test]
fn restore_refuses_a_file_with_no_schema_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    SqliteStore::open(&live).unwrap().close().unwrap();
    let junk = dir.path().join("junk.db");
    std::fs::write(&junk, b"not a database").unwrap();

    let err = SqliteStore::restore_from(&junk, &live).unwrap_err();
    assert!(err.to_string().contains("ledger"), "{err}");

    // Live still opens: the failed validation staged nothing.
    assert!(!dir.path().join(".restore.tmp").exists());
    SqliteStore::open(&live).unwrap().close().unwrap();
}

#[test]
fn restore_refuses_live_wal_sidecars_without_touching_live() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store);
    let backup = dir.path().join("actions.backup.db");
    store.backup_to(&backup).unwrap();
    // Leave un-checkpointed WAL activity beside live through a second
    // connection (the store handle stays open, so the sidecar persists).
    let raw = AsyncConnection::open_sync(live.to_string_lossy().into_owned()).unwrap();
    raw.execute_sync("PRAGMA journal_mode=WAL").unwrap();
    raw.execute_sync("CREATE TABLE wal_probe(x TEXT)").unwrap();
    drop(raw);
    assert!(
        dir.path().join("actions.db-wal").exists(),
        "probe must leave a WAL sidecar"
    );

    let err = SqliteStore::restore_from(&backup, &live).unwrap_err();
    assert!(err.to_string().contains("checkpoint"), "{err}");

    // Refused before staging: no temp file, live data intact via the handle.
    assert!(!dir.path().join(".restore.tmp").exists());
    assert_eq!(store.list_devices().unwrap().len(), 1);
    store.close().unwrap();
}
