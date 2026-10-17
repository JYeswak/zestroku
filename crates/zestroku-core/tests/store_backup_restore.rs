//! Backup/restore for upgrade rollback and recovery (1r4.9).
//!
//! Real fsqlite temp-file databases; no mocks. Device identifiers below are
//! synthetic placeholders, never real serials/UDNs/MACs. Every temporary
//! connection is JOINED (`close_sync` / `close_without_checkpoint_sync`):
//! dropping an `AsyncConnection` only detaches its worker, so a dropped
//! handle is not a closed handle.

use std::path::Path;

use fsqlite::{AsyncConnection, SqliteValue};
use zestroku_core::store::{ActionRow, AliasRow, DeviceRow, MIGRATIONS, SqliteStore, Store};

/// The WAL file header size; a checkpointed WAL keeps exactly this much.
const WAL_HEADER_BYTES: u64 = 32;

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

/// Live marker one: a device, an alias and a history row.
fn seed(store: &SqliteStore) {
    store.upsert_device(&device("dev-1")).unwrap();
    store
        .set_alias(&AliasRow {
            alias: "tv".into(),
            device_id: "dev-1".into(),
        })
        .unwrap();
    store.insert_action(&action()).unwrap();
}

fn ids(store: &SqliteStore) -> Vec<String> {
    let mut ids: Vec<String> = store
        .list_devices()
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    ids.sort();
    ids
}

fn insert_sql(id: &str) -> String {
    format!(
        "INSERT INTO devices (id, name, ip, model, model_num, firmware, wifi_driver, \
         access_tier, last_seen) VALUES ('{id}', 'Raw', '192.0.2.99', 'm', 'n', 'f', 'd', \
         'permissive', 1700000001)"
    )
}

fn path_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Put the database at `path` in WAL mode on `conn` and prove it.
fn enter_wal_mode(conn: &AsyncConnection) {
    conn.execute_sync("PRAGMA journal_mode=WAL").unwrap();
    let rows = conn
        .query_with_params_sync("PRAGMA journal_mode", &[])
        .unwrap();
    assert_eq!(
        rows[0].get(0).and_then(SqliteValue::as_text),
        Some("wal"),
        "the recovery scenarios are WAL scenarios"
    );
}

fn wal_len(live: &Path) -> u64 {
    let mut wal = live.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::metadata(Path::new(&wal)).map_or(0, |m| m.len())
}

fn assert_no_leftovers(dir: &Path, name: &str) {
    assert!(!dir.join(format!("{name}-wal")).exists(), "stale WAL");
    assert!(!dir.join(format!("{name}-shm")).exists(), "stale shm");
    assert!(!dir.join(".restore.tmp").exists(), "staging file");
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
    assert_eq!(ids(&snap), ["dev-1"]);
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
    // Post-backup drift: live now holds two devices, the backup one.
    store.upsert_device(&device("dev-2")).unwrap();
    store.close().unwrap();
    let backup_before = std::fs::read(&backup).unwrap();

    SqliteStore::restore_from(&backup, &live).unwrap();

    assert_no_leftovers(dir.path(), "actions.db");
    assert_eq!(std::fs::read(&backup).unwrap(), backup_before);
    let rolled = SqliteStore::open(&live).unwrap();
    assert_eq!(ids(&rolled), ["dev-1"], "drift rolled back");
    assert_eq!(rolled.list_aliases().unwrap().len(), 1);
    assert_eq!(rolled.list_actions().unwrap().len(), 1);
    rolled.close().unwrap();
}

#[test]
fn restore_checkpoints_durable_wal_when_every_handle_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store);
    let backup = dir.path().join("actions.backup.db");
    store.backup_to(&backup).unwrap();
    store.close().unwrap();

    // Durable, committed frames in the live WAL with NO open handle: the
    // writer is joined without a close-time checkpoint, so the frames stay.
    let mut raw = AsyncConnection::open_sync(path_of(&live)).unwrap();
    enter_wal_mode(&raw);
    raw.execute_sync(&insert_sql("dev-wal")).unwrap();
    raw.close_without_checkpoint_sync().unwrap();
    assert!(
        wal_len(&live) > WAL_HEADER_BYTES,
        "the fixture must leave frames beyond the WAL header"
    );

    SqliteStore::restore_from(&backup, &live).unwrap();

    assert_no_leftovers(dir.path(), "actions.db");
    let rolled = SqliteStore::open(&live).unwrap();
    assert_eq!(ids(&rolled), ["dev-1"], "live (two rows) became the backup");
    rolled.close().unwrap();
}

#[test]
fn restore_refuses_an_active_writer_even_with_zero_wal_frames() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store); // live marker one
    let backup = dir.path().join("actions.backup.db");
    store.backup_to(&backup).unwrap(); // backup: one device
    store.upsert_device(&device("dev-2")).unwrap(); // live marker two
    store.close().unwrap(); // joined and checkpointed: no WAL frames
    let backup_before = std::fs::read(&backup).unwrap();

    // Active writer: holds the write lock but has written nothing yet.
    let mut writer = AsyncConnection::open_sync(path_of(&live)).unwrap();
    enter_wal_mode(&writer);
    writer.execute_sync("BEGIN IMMEDIATE").unwrap();
    assert!(
        wal_len(&live) <= WAL_HEADER_BYTES,
        "zero-frame precondition: the byte count alone cannot see this writer"
    );

    let err = SqliteStore::restore_from(&backup, &live).unwrap_err();
    assert!(err.to_string().contains("busy=1"), "{err}");
    assert!(!dir.path().join(".restore.tmp").exists(), "no staging");
    assert_eq!(std::fs::read(&backup).unwrap(), backup_before);

    // The held write then COMMITS: a wrongly promoted backup would have
    // swapped the database beneath it and lost one or both markers.
    writer.execute_sync(&insert_sql("dev-held")).unwrap();
    writer.execute_sync("COMMIT").unwrap();
    writer.close_sync().unwrap();

    let reopened = SqliteStore::open(&live).unwrap();
    assert_eq!(ids(&reopened), ["dev-1", "dev-2", "dev-held"]);
    reopened.close().unwrap();
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
    let mut conn = AsyncConnection::open_sync(path_of(&backup)).unwrap();
    let max = MIGRATIONS.iter().map(|m| m.version).max().unwrap();
    conn.execute_with_params_sync(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
        &[
            SqliteValue::from(max + 1),
            SqliteValue::from("forged-future"),
            SqliteValue::from(1_700_000_200i64),
        ],
    )
    .unwrap();
    conn.close_sync().unwrap();

    let err = SqliteStore::restore_from(&backup, &live).unwrap_err();
    assert!(err.to_string().contains("newer"), "{err}");

    assert!(!dir.path().join(".restore.tmp").exists());
    let kept = SqliteStore::open(&live).unwrap();
    assert_eq!(ids(&kept), ["dev-1"]);
    kept.close().unwrap();
}

#[test]
fn restore_refuses_malformed_bytes_and_keeps_live() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store);
    store.close().unwrap();
    // Fourteen bytes: shorter than any SQLite header.
    let junk = dir.path().join("junk.db");
    std::fs::write(&junk, b"not a database").unwrap();

    let err = SqliteStore::restore_from(&junk, &live).unwrap_err();
    assert!(err.to_string().contains("not a SQLite database"), "{err}");

    assert!(!dir.path().join(".restore.tmp").exists());
    let kept = SqliteStore::open(&live).unwrap();
    assert_eq!(ids(&kept), ["dev-1"]);
    kept.close().unwrap();
}

#[test]
fn restore_refuses_a_valid_database_missing_its_ledger_and_keeps_live() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("actions.db");
    let store = SqliteStore::open(&live).unwrap();
    seed(&store);
    store.close().unwrap();
    // A well-formed SQLite file (header passes) that is not a zestroku
    // store: distinct from malformed bytes.
    let bare = dir.path().join("bare.db");
    let mut raw = AsyncConnection::open_sync(path_of(&bare)).unwrap();
    raw.execute_sync("CREATE TABLE other(x TEXT)").unwrap();
    raw.close_sync().unwrap();

    let err = SqliteStore::restore_from(&bare, &live).unwrap_err();
    assert!(err.to_string().contains("no schema ledger"), "{err}");

    assert!(!dir.path().join(".restore.tmp").exists());
    let kept = SqliteStore::open(&live).unwrap();
    assert_eq!(ids(&kept), ["dev-1"]);
    kept.close().unwrap();
}

#[cfg(unix)]
#[test]
fn restore_fails_closed_when_the_live_path_cannot_be_statted() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.db");
    let store = SqliteStore::open(&source).unwrap();
    seed(&store);
    let backup = dir.path().join("actions.backup.db");
    store.backup_to(&backup).unwrap();
    store.close().unwrap();
    // A regular file used as a directory: stat fails with ENOTDIR, which is
    // not NotFound, so the restore must refuse rather than assume absence.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"file").unwrap();
    let live = blocker.join("actions.db");

    let err = SqliteStore::restore_from(&backup, &live).unwrap_err();
    assert!(
        err.to_string().contains("cannot stat live database"),
        "{err}"
    );
    assert_eq!(std::fs::read(&blocker).unwrap(), b"file");
}
