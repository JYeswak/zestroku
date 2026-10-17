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
    assert!(err.to_string().contains("not a SQLite database"), "{err}");

    // Live still opens: the failed validation staged nothing.
    assert!(!dir.path().join(".restore.tmp").exists());
    SqliteStore::open(&live).unwrap().close().unwrap();
}

#[test]
fn restore_checkpoint_consolidates_live_wal_then_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store);
    let backup = dir.path().join("actions.backup.db");
    store.backup_to(&backup).unwrap();
    // Upgrade flow order: the store handle is closed first (daemon stopped).
    // A second connection then leaves un-checkpointed WAL beside live and
    // stays open, so the sidecar deterministically persists on every
    // platform (close alone does not remove it on macOS).
    store.close().unwrap();
    let raw = AsyncConnection::open_sync(live.to_string_lossy().into_owned()).unwrap();
    raw.execute_sync("PRAGMA journal_mode=WAL").unwrap();
    raw.execute_sync("CREATE TABLE wal_probe(x TEXT)").unwrap();
    assert!(
        dir.path().join("actions.db-wal").exists(),
        "probe must leave a WAL sidecar"
    );
    // Record pre-restore sizes: the WAL must hold bytes (proving restore
    // consolidates rather than taking the trivial path).
    let wal_bytes = std::fs::metadata(dir.path().join("actions.db-wal"))
        .unwrap()
        .len();
    assert!(wal_bytes > 0, "probe WAL must be nonempty, got {wal_bytes}");

    // Restore checkpoints (preserving committed data) instead of refusing.
    SqliteStore::restore_from(&backup, &live).unwrap();

    // Promoted snapshot is self-contained: no sidecars, no staging file.
    assert!(!dir.path().join("actions.db-wal").exists());
    assert!(!dir.path().join("actions.db-shm").exists());
    assert!(!dir.path().join(".restore.tmp").exists());
    drop(raw);
    let rolled = SqliteStore::open(&live).unwrap();
    assert_eq!(rolled.list_devices().unwrap().len(), 1);
    rolled.close().unwrap();
}

#[test]
fn restore_refuses_wal_bytes_no_checkpoint_can_consolidate() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store);
    let backup = dir.path().join("actions.backup.db");
    store.backup_to(&backup).unwrap();
    store.close().unwrap();
    // An uncommitted writer holds bytes in the WAL that no checkpoint may
    // consolidate; the handle stays open so the sidecar persists.
    let raw = AsyncConnection::open_sync(live.to_string_lossy().into_owned()).unwrap();
    raw.execute_sync("PRAGMA journal_mode=WAL").unwrap();
    raw.execute_sync("CREATE TABLE wal_probe(x TEXT)").unwrap();
    raw.execute_sync("BEGIN IMMEDIATE").unwrap();
    raw.execute_sync("INSERT INTO wal_probe VALUES ('held')")
        .unwrap();
    assert!(dir.path().join("actions.db-wal").exists());

    let err = SqliteStore::restore_from(&backup, &live).unwrap_err();
    assert!(err.to_string().contains("un-checkpointed"), "{err}");

    // Refused before staging: the held write is untouched, live is intact.
    assert!(!dir.path().join(".restore.tmp").exists());
    raw.execute_sync("ROLLBACK").unwrap();
    drop(raw);
    let kept = SqliteStore::open(&live).unwrap();
    assert_eq!(kept.list_devices().unwrap().len(), 1);
    kept.close().unwrap();
}

#[test]
fn restore_fails_closed_when_the_wal_cannot_be_statted() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store);
    let backup = dir.path().join("actions.backup.db");
    store.backup_to(&backup).unwrap();
    store.close().unwrap();
    // A regular file where the WAL would live: stat fails with ENOTDIR
    // (deterministic without permission games), never NotFound.
    std::fs::write(dir.path().join("actions.db-wal"), b"blocker").unwrap();
    let blocker_live = dir.path().join("actions.db-wal").join("actions.db");
    let err = SqliteStore::restore_from(&backup, &blocker_live).unwrap_err();
    assert!(err.to_string().contains("cannot stat"), "{err}");
}

#[test]
fn restore_names_a_valid_database_missing_its_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    SqliteStore::open(&live).unwrap().close().unwrap();
    // Valid SQLite, but no schema_migrations table: distinct from
    // malformed bytes (which fail the header check instead).
    let bare = dir.path().join("bare.db");
    let raw = AsyncConnection::open_sync(bare.to_string_lossy().into_owned()).unwrap();
    raw.execute_sync("CREATE TABLE other(x TEXT)").unwrap();
    drop(raw);
    let err = SqliteStore::restore_from(&bare, &live).unwrap_err();
    assert!(err.to_string().contains("no schema ledger"), "{err}");
    assert!(!dir.path().join(".restore.tmp").exists());
    SqliteStore::open(&live).unwrap().close().unwrap();
}
