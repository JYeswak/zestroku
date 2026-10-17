//! Durable store: fsqlite `AsyncConnection` (a `Send` handle over a dedicated
//! large-stack worker), driven through its `*_sync` methods. Never hold a
//! transaction across network I/O; every method here is one short statement
//! or one short transaction.

pub(crate) mod migrations;

use std::fmt::Display;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use fsqlite::{AsyncConnection, FrankenError, Row, SqliteValue};

pub use migrations::{MIGRATIONS, Migration, is_contiguous};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store backend: {0}")]
    Backend(String),
}

fn backend(e: impl Display) -> StoreError {
    StoreError::Backend(e.to_string())
}
fn transaction_error(primary: FrankenError, rollback: Result<(), FrankenError>) -> StoreError {
    match rollback {
        Ok(()) => backend(primary),
        Err(rollback) => backend(format!(
            "{primary}; transaction rollback cleanup failed: {rollback}"
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub model: String,
    pub model_num: String,
    pub firmware: String,
    pub wifi_driver: String,
    pub access_tier: String,
    pub last_seen: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRow {
    pub device_id: String,
    pub app_id: String,
    pub name: String,
    pub version: String,
    pub fetched_at: i64,
}

/// Per-device probe result; `probe_map` is JSON: command class -> open/403/404/untested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessTierRow {
    pub device_id: String,
    pub tier: String,
    pub probe_at: i64,
    pub probe_map: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRow {
    /// Assigned by the store on insert; ignored in `insert_action`.
    pub id: i64,
    pub at: i64,
    pub client: String,
    pub surface: String,
    pub intent: String,
    pub decision: String,
    pub result: String,
    pub before_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    /// Assigned by the store on insert; ignored in `insert_diff`.
    pub id: i64,
    pub left_device: String,
    pub right_device: String,
    pub surface: String,
    pub field: String,
    pub left_value: String,
    pub right_value: String,
    pub ran_at: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationTokenRow {
    pub token_hash: String,
    pub device_id: String,
    pub action: String,
    /// Canonical client binding; `None` is preserved for legacy grants.
    pub client: Option<String>,
    pub expires_at: i64,
}

/// Client binding and expiry read atomically from one approval row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationGrant {
    pub client: Option<String>,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasRow {
    pub alias: String,
    pub device_id: String,
}
pub trait Store {
    fn schema_versions(&self) -> Result<Vec<i64>, StoreError>;

    fn upsert_device(&self, d: &DeviceRow) -> Result<(), StoreError>;
    fn get_device(&self, id: &str) -> Result<Option<DeviceRow>, StoreError>;
    fn list_devices(&self) -> Result<Vec<DeviceRow>, StoreError>;
    /// Deletes the device and every dependent row (apps, capabilities, access_tier).
    fn delete_device(&self, id: &str) -> Result<(), StoreError>;

    fn upsert_app(&self, a: &AppRow) -> Result<(), StoreError>;
    fn list_apps(&self, device_id: &str) -> Result<Vec<AppRow>, StoreError>;
    fn delete_app(&self, device_id: &str, app_id: &str) -> Result<(), StoreError>;

    fn add_capability(&self, device_id: &str, cap: &str) -> Result<(), StoreError>;
    fn list_capabilities(&self, device_id: &str) -> Result<Vec<String>, StoreError>;
    fn remove_capability(&self, device_id: &str, cap: &str) -> Result<(), StoreError>;
    fn set_alias(&self, alias: &AliasRow) -> Result<(), StoreError>;
    fn list_aliases(&self) -> Result<Vec<AliasRow>, StoreError>;
    fn delete_alias(&self, alias: &str) -> Result<(), StoreError>;
    /// Record the stable identity key for a device id (see
    /// `discovery::identity_key`); the key is a digest, never raw serials.
    fn record_device_identity(&self, device_id: &str, identity: &str) -> Result<(), StoreError>;
    /// Find the device id currently holding an identity key, if any.
    fn find_device_id_by_identity(&self, identity: &str) -> Result<Option<String>, StoreError>;
    /// Forget one device id's identity record (used when its row moves).
    fn delete_device_identity(&self, device_id: &str) -> Result<(), StoreError>;
    /// Re-point alias rows at a moved device id (DHCP move follow-up).
    fn rekey_aliases(&self, old_id: &str, new_id: &str) -> Result<(), StoreError>;
    /// Move a known device identity to a new endpoint as one durable state change.
    fn move_device_identity(
        &self,
        old_id: &str,
        new_device: &DeviceRow,
        identity: &str,
    ) -> Result<(), StoreError>;
    fn insert_confirmation_token(&self, token: &ConfirmationTokenRow) -> Result<(), StoreError>;
    fn consume_confirmation_token(
        &self,
        token_hash: &str,
        device_id: &str,
        action: &str,
        client: &str,
        now: i64,
    ) -> Result<bool, StoreError>;

    fn upsert_access_tier(&self, t: &AccessTierRow) -> Result<(), StoreError>;
    fn get_access_tier(&self, device_id: &str) -> Result<Option<AccessTierRow>, StoreError>;
    fn delete_access_tier(&self, device_id: &str) -> Result<(), StoreError>;

    /// Returns the assigned id.
    fn insert_action(&self, a: &ActionRow) -> Result<i64, StoreError>;
    fn get_action(&self, id: i64) -> Result<Option<ActionRow>, StoreError>;
    /// Atomic rate-limit gate: prune hits older than an hour, count
    /// in-window hits, insert on budget and return true; return false
    /// without inserting when exhausted. One transaction: safe across
    /// separate CLI processes and connections (no TOCTOU).
    fn try_record_policy_hit(
        &self,
        client: &str,
        verb: &str,
        since: i64,
        budget: i64,
        now: i64,
    ) -> Result<bool, StoreError>;
    /// Non-consuming typed grant lookup; client and expiry come from one row.
    fn peek_confirmation_grant(
        &self,
        token_hash: &str,
    ) -> Result<Option<ConfirmationGrant>, StoreError>;
    /// Expiry-only lookup retained for quiet-hours store checks.
    /// Principal-aware authorization must use `peek_confirmation_grant`.
    fn peek_confirmation_token_expiry(&self, token_hash: &str) -> Result<Option<i64>, StoreError>;
    fn list_actions(&self) -> Result<Vec<ActionRow>, StoreError>;
    fn update_action_result(&self, id: i64, result: &str) -> Result<(), StoreError>;
    fn delete_action(&self, id: i64) -> Result<(), StoreError>;

    /// Returns the assigned id.
    fn insert_diff(&self, d: &DiffRow) -> Result<i64, StoreError>;
    fn list_diffs(&self) -> Result<Vec<DiffRow>, StoreError>;
    fn delete_diff(&self, id: i64) -> Result<(), StoreError>;
}

#[derive(Debug)]
pub struct SqliteStore {
    conn: AsyncConnection,
}

impl SqliteStore {
    /// Open (creating if needed) the database at `path` and migrate it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with(path, MIGRATIONS)
    }

    /// Open with an explicit migration list (used to prove append-only upgrades).
    pub fn open_with(path: &Path, migrations: &[Migration]) -> Result<Self, StoreError> {
        if !is_contiguous(migrations) {
            return Err(backend("migration versions must be 1..=N, contiguous"));
        }
        let store = Self {
            conn: AsyncConnection::open_sync(path.to_string_lossy().into_owned())
                .map_err(backend)?,
        };
        store.migrate(migrations)?;
        Ok(store)
    }

    /// Close, checkpointing the WAL.
    pub fn close(mut self) -> Result<(), StoreError> {
        self.conn.close_sync().map_err(backend)
    }
    /// Write a consistent snapshot of the live database to `dest`.
    ///
    /// Uses `VACUUM INTO`: driver-supported, runs on the live connection,
    /// and produces a de-fragmented consistent copy (no stock copy client,
    /// no WAL fragments). `dest` is overwritten when present; its parent
    /// directory must exist.
    pub fn backup_to(&self, dest: &Path) -> Result<(), StoreError> {
        let escaped = dest.to_string_lossy().replace('\'', "''");
        self.conn
            .execute_sync(&format!("VACUUM INTO '{escaped}'"))
            .map(|_| ())
            .map_err(backend)
    }

    /// Atomically replace the live database at `live` with the backup at
    /// `backup`, for upgrade rollback and recovery (1r4.9).
    ///
    /// The caller must have closed every handle on `live` (back up first,
    /// stop the daemon through the resident route). Validation first: the
    /// backup must be a SQLite database carrying a schema ledger whose max
    /// version this binary supports — a newer-schema backup is refused,
    /// never migrated in place and never silently discarded (1r4.7 refusal,
    /// extended to restore). Live WAL sidecars are checkpointed into the
    /// live database before promotion (checkpointing preserves committed
    /// runtime data; only a sidecar that survives checkpointing is refused,
    /// never deleted). Promotion is copy-to-temp plus rename, so an
    /// interrupted restore leaves `live` untouched: recover by retrying the
    /// restore or keeping the pre-upgrade backup.
    pub fn restore_from(backup: &Path, live: &Path) -> Result<(), StoreError> {
        let max = applied_schema_max(backup)?;
        let supported = MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0);
        if max > supported {
            return Err(backend(format!(
                "backup schema version {max} is newer than this binary supports \
                 ({supported}); refusing restore (restore with a newer binary)"
            )));
        }
        let parent = live
            .parent()
            .ok_or_else(|| backend("live path has no parent"))?;
        checkpoint_live(live)?;
        // Only a sidecar that survives checkpointing is refused: it may hold
        // data no checkpoint could consolidate. Never delete it here.
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = live.as_os_str().to_owned();
            sidecar.push(suffix);
            if Path::new(&sidecar).exists() {
                return Err(backend(format!(
                    "live WAL sidecar {} survives checkpointing; refusing restore",
                    Path::new(&sidecar).display()
                )));
            }
        }
        let tmp = parent.join(".restore.tmp");
        std::fs::copy(backup, &tmp).map_err(|e| backend(format!("copy backup to staging: {e}")))?;
        std::fs::rename(&tmp, live).map_err(|e| backend(format!("promote staged restore: {e}")))?;
        Ok(())
    }

    fn migrate(&self, migrations: &[Migration]) -> Result<(), StoreError> {
        self.conn
            .execute_sync(
                "CREATE TABLE IF NOT EXISTS schema_migrations (\
                 version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL)",
            )
            .map_err(backend)?;
        let applied = self.schema_versions()?;
        // Upgrade safety (1r4.7): an old binary must never migrate or write
        // a database whose schema is newer than its supported catalog.
        // Fail closed before any migration or data write.
        let supported = migrations.iter().map(|m| m.version).max().unwrap_or(0);
        if let Some(&max) = applied.iter().max()
            && max > supported
        {
            return Err(backend(format!(
                "store schema version {max} is newer than this binary supports \
                 ({supported}); refusing to migrate or write (open with a newer binary)"
            )));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        for m in migrations.iter().filter(|m| !applied.contains(&m.version)) {
            self.in_transaction(|c| {
                c.execute_batch_sync(m.sql)?;
                c.execute_with_params_sync(
                    "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                    &[m.version.into(), m.name.into(), now.into()],
                )
                .map(drop)
            })
            .map_err(|e| backend(format!("migration {} ({}): {e}", m.version, m.name)))?;
        }
        Ok(())
    }

    fn in_transaction<T>(
        &self,
        f: impl FnOnce(&AsyncConnection) -> Result<T, FrankenError>,
    ) -> Result<T, StoreError> {
        self.conn.begin_transaction_sync().map_err(backend)?;
        match f(&self.conn) {
            Ok(v) => match self.conn.commit_transaction_sync() {
                Ok(()) => Ok(v),
                Err(error) => Err(self.cleanup_transaction_error(error)),
            },
            Err(error) => Err(self.cleanup_transaction_error(error)),
        }
    }

    // fsqlite may leave failed commits active; other failures may already have rolled back.
    // Clean only an active transaction, retaining cleanup errors beside the primary error.
    fn cleanup_transaction_error(&self, primary: FrankenError) -> StoreError {
        let rollback = if self.conn.in_transaction() {
            self.conn.rollback_transaction_sync()
        } else {
            Ok(())
        };
        transaction_error(primary, rollback)
    }

    fn query(&self, sql: &str, params: &[SqliteValue]) -> Result<Vec<Row>, StoreError> {
        self.conn
            .query_with_params_sync(sql, params)
            .map_err(backend)
    }

    fn exec(&self, sql: &str, params: &[SqliteValue]) -> Result<(), StoreError> {
        self.conn
            .execute_with_params_sync(sql, params)
            .map(drop)
            .map_err(backend)
    }
}

fn delete_device_rows(conn: &AsyncConnection, id: &str) -> Result<(), FrankenError> {
    for sql in [
        "DELETE FROM apps WHERE device_id = ?1",
        "DELETE FROM capabilities WHERE device_id = ?1",
        "DELETE FROM access_tier WHERE device_id = ?1",
        "DELETE FROM aliases WHERE device_id = ?1",
        "DELETE FROM devices WHERE id = ?1",
    ] {
        conn.execute_with_params_sync(sql, &[s(id)])?;
    }
    Ok(())
}

fn upsert_device_row(conn: &AsyncConnection, device: &DeviceRow) -> Result<(), FrankenError> {
    conn.execute_with_params_sync(
        &format!(
            "INSERT OR REPLACE INTO devices ({DEVICE_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"
        ),
        &[
            s(&device.id),
            s(&device.name),
            s(&device.ip),
            s(&device.model),
            s(&device.model_num),
            s(&device.firmware),
            s(&device.wifi_driver),
            s(&device.access_tier),
            i(device.last_seen),
        ],
    )
    .map(drop)
}

fn text(row: &Row, i: usize) -> Result<String, StoreError> {
    row.get(i)
        .and_then(SqliteValue::as_text)
        .map(str::to_owned)
        .ok_or_else(|| backend(format!("column {i} is not TEXT")))
}

fn int(row: &Row, i: usize) -> Result<i64, StoreError> {
    row.get(i)
        .and_then(SqliteValue::as_integer)
        .ok_or_else(|| backend(format!("column {i} is not INTEGER")))
}
/// Max applied schema version in the database at `path`, read without
/// migrating (restore validation must never mutate the backup). A missing
/// ledger means the file is not a zestroku store: refuse, do not assume.
/// A file without the SQLite header is refused before any SQL runs, so the
/// error names the cause instead of leaking driver diagnostics.
fn applied_schema_max(path: &Path) -> Result<i64, StoreError> {
    use std::io::Read as _;
    let mut header = [0u8; 16];
    std::fs::File::open(path)
        .map_err(|e| backend(format!("backup is not readable: {e}")))?
        .read_exact(&mut header)
        .map_err(|e| {
            backend(format!(
                "backup {} is not a SQLite database: {e}",
                path.display()
            ))
        })?;
    if header != *b"SQLite format 3\0" {
        return Err(backend(format!(
            "backup {} is not a SQLite database",
            path.display()
        )));
    }
    let conn = AsyncConnection::open_sync(path.to_string_lossy().into_owned()).map_err(backend)?;
    let rows = conn
        .query_with_params_sync("SELECT version FROM schema_migrations", &[])
        .map_err(|e| backend(format!("backup has no schema ledger: {e}")))?;
    rows.iter()
        .map(|r| {
            r.get(0)
                .and_then(SqliteValue::as_integer)
                .ok_or_else(|| backend("schema ledger version is not INTEGER"))
        })
        .try_fold(0i64, |max, v| v.map(|v| max.max(v)))
}

/// Consolidate live WAL sidecars into the live database (preserving every
/// committed byte) so promotion replaces one self-contained file. A missing
/// live file needs no checkpoint. Checkpoint errors are tolerated: the
/// post-checkpoint sidecar recheck refuses what survives.
fn checkpoint_live(live: &Path) -> Result<(), StoreError> {
    if !live.exists() {
        return Ok(());
    }
    let conn = AsyncConnection::open_sync(live.to_string_lossy().into_owned())
        .map_err(|e| backend(format!("cannot open live database for checkpoint: {e}")))?;
    let _ = conn.execute_sync("PRAGMA wal_checkpoint(TRUNCATE)");
    Ok(())
}

fn s(v: &str) -> SqliteValue {
    SqliteValue::from(v)
}

fn i(v: i64) -> SqliteValue {
    SqliteValue::from(v)
}

fn device(r: &Row) -> Result<DeviceRow, StoreError> {
    Ok(DeviceRow {
        id: text(r, 0)?,
        name: text(r, 1)?,
        ip: text(r, 2)?,
        model: text(r, 3)?,
        model_num: text(r, 4)?,
        firmware: text(r, 5)?,
        wifi_driver: text(r, 6)?,
        access_tier: text(r, 7)?,
        last_seen: int(r, 8)?,
    })
}

const DEVICE_COLS: &str =
    "id, name, ip, model, model_num, firmware, wifi_driver, access_tier, last_seen";
const ACTION_COLS: &str = "id, at, client, surface, intent, decision, result, before_state";
const DIFF_COLS: &str =
    "id, left_device, right_device, surface, field, left_value, right_value, ran_at";

fn action(r: &Row) -> Result<ActionRow, StoreError> {
    Ok(ActionRow {
        id: int(r, 0)?,
        at: int(r, 1)?,
        client: text(r, 2)?,
        surface: text(r, 3)?,
        intent: text(r, 4)?,
        decision: text(r, 5)?,
        result: text(r, 6)?,
        before_state: text(r, 7)?,
    })
}

fn diff(r: &Row) -> Result<DiffRow, StoreError> {
    Ok(DiffRow {
        id: int(r, 0)?,
        left_device: text(r, 1)?,
        right_device: text(r, 2)?,
        surface: text(r, 3)?,
        field: text(r, 4)?,
        left_value: text(r, 5)?,
        right_value: text(r, 6)?,
        ran_at: int(r, 7)?,
    })
}

impl Store for SqliteStore {
    fn schema_versions(&self) -> Result<Vec<i64>, StoreError> {
        self.query(
            "SELECT version FROM schema_migrations ORDER BY version",
            &[],
        )?
        .iter()
        .map(|r| int(r, 0))
        .collect()
    }

    fn set_alias(&self, alias: &AliasRow) -> Result<(), StoreError> {
        self.exec(
            "INSERT OR REPLACE INTO aliases (alias, device_id) VALUES (?1, ?2)",
            &[s(&alias.alias), s(&alias.device_id)],
        )
    }

    fn list_aliases(&self) -> Result<Vec<AliasRow>, StoreError> {
        self.query("SELECT alias, device_id FROM aliases ORDER BY alias", &[])?
            .iter()
            .map(|row| {
                Ok(AliasRow {
                    alias: text(row, 0)?,
                    device_id: text(row, 1)?,
                })
            })
            .collect()
    }

    fn delete_alias(&self, alias: &str) -> Result<(), StoreError> {
        self.exec("DELETE FROM aliases WHERE alias = ?1", &[s(alias)])
    }
    fn record_device_identity(&self, device_id: &str, identity: &str) -> Result<(), StoreError> {
        self.exec(
            "INSERT OR REPLACE INTO device_identity (device_id, identity) VALUES (?1, ?2)",
            &[s(device_id), s(identity)],
        )
    }

    fn find_device_id_by_identity(&self, identity: &str) -> Result<Option<String>, StoreError> {
        self.query(
            "SELECT device_id FROM device_identity WHERE identity = ?1 LIMIT 1",
            &[s(identity)],
        )?
        .first()
        .map(|row| text(row, 0))
        .transpose()
    }

    fn delete_device_identity(&self, device_id: &str) -> Result<(), StoreError> {
        self.exec(
            "DELETE FROM device_identity WHERE device_id = ?1",
            &[s(device_id)],
        )
    }

    fn rekey_aliases(&self, old_id: &str, new_id: &str) -> Result<(), StoreError> {
        self.exec(
            "UPDATE aliases SET device_id = ?2 WHERE device_id = ?1",
            &[s(old_id), s(new_id)],
        )
    }

    fn move_device_identity(
        &self,
        old_id: &str,
        new_device: &DeviceRow,
        identity: &str,
    ) -> Result<(), StoreError> {
        let moved = self.in_transaction(|conn| {
            // Occupancy gate before any writes: a destination identity
            // record owned by anyone but the moving identity refuses the
            // move, so the INSERT OR REPLACE below can never merge two
            // identities -- even when the destination has an identity
            // record but no device row (orphan mapping), or a row with no
            // identity. A destination already carrying the moving identity
            // (retry) proceeds.
            let same_occupant = !conn
                .query_with_params_sync(
                    "SELECT device_id FROM device_identity WHERE device_id = ?1 AND identity = ?2 LIMIT 1",
                    &[s(&new_device.id), s(identity)],
                )?
                .is_empty();
            if !same_occupant {
                let foreign_identity = !conn
                    .query_with_params_sync(
                        "SELECT device_id FROM device_identity WHERE device_id = ?1 LIMIT 1",
                        &[s(&new_device.id)],
                    )?
                    .is_empty();
                let foreign_row = !conn
                    .query_with_params_sync(
                        "SELECT id FROM devices WHERE id = ?1 LIMIT 1",
                        &[s(&new_device.id)],
                    )?
                    .is_empty();
                if foreign_identity || foreign_row {
                    return Ok(false);
                }
            }
            conn.execute_with_params_sync(
                "UPDATE aliases SET device_id = ?2 WHERE device_id = ?1",
                &[s(old_id), s(&new_device.id)],
            )
            .map(drop)?;
            delete_device_rows(conn, old_id)?;
            conn.execute_with_params_sync(
                "DELETE FROM device_identity WHERE device_id = ?1",
                &[s(old_id)],
            )
            .map(drop)?;
            upsert_device_row(conn, new_device)?;
            conn.execute_with_params_sync(
                "INSERT OR REPLACE INTO device_identity (device_id, identity) VALUES (?1, ?2)",
                &[s(&new_device.id), s(identity)],
            )
            .map(drop)?;
            Ok(true)
        })?;
        if !moved {
            return Err(backend(format!(
                "destination {} already owned by a distinct identity",
                new_device.id
            )));
        }
        Ok(())
    }

    fn insert_confirmation_token(&self, token: &ConfirmationTokenRow) -> Result<(), StoreError> {
        self.exec(
            "INSERT INTO confirmation_tokens (token_hash, device_id, action, client, expires_at, consumed_at) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            &[
                s(&token.token_hash),
                s(&token.device_id),
                s(&token.action),
                token.client.as_deref().map(s).unwrap_or(SqliteValue::Null),
                i(token.expires_at),
            ],
        )
    }

    fn consume_confirmation_token(
        &self,
        token_hash: &str,
        device_id: &str,
        action: &str,
        client: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        self.in_transaction(|conn| {
            let rows = conn.query_with_params_sync(
                "SELECT token_hash FROM confirmation_tokens WHERE token_hash = ?1 AND device_id = ?2 AND action = ?3 AND client = ?4 AND expires_at > ?5 AND consumed_at IS NULL",
                &[s(token_hash), s(device_id), s(action), s(client), i(now)],
            )?;
            if rows.is_empty() { return Ok(false); }
            conn.execute_with_params_sync(
                "UPDATE confirmation_tokens SET consumed_at = ?5 WHERE token_hash = ?1 AND device_id = ?2 AND action = ?3 AND client = ?4 AND expires_at > ?5 AND consumed_at IS NULL",
                &[s(token_hash), s(device_id), s(action), s(client), i(now)],
            ).map(drop)?;
            Ok(true)
        })
    }

    fn upsert_device(&self, d: &DeviceRow) -> Result<(), StoreError> {
        upsert_device_row(&self.conn, d).map_err(backend)
    }

    fn get_device(&self, id: &str) -> Result<Option<DeviceRow>, StoreError> {
        self.query(
            &format!("SELECT {DEVICE_COLS} FROM devices WHERE id = ?1"),
            &[s(id)],
        )?
        .first()
        .map(device)
        .transpose()
    }

    fn list_devices(&self) -> Result<Vec<DeviceRow>, StoreError> {
        self.query(
            &format!("SELECT {DEVICE_COLS} FROM devices ORDER BY id"),
            &[],
        )?
        .iter()
        .map(device)
        .collect()
    }

    fn delete_device(&self, id: &str) -> Result<(), StoreError> {
        self.in_transaction(|conn| delete_device_rows(conn, id))
    }

    fn upsert_app(&self, a: &AppRow) -> Result<(), StoreError> {
        self.exec(
            "INSERT OR REPLACE INTO apps (device_id, app_id, name, version, fetched_at) \
             VALUES (?1,?2,?3,?4,?5)",
            &[
                s(&a.device_id),
                s(&a.app_id),
                s(&a.name),
                s(&a.version),
                i(a.fetched_at),
            ],
        )
    }

    fn list_apps(&self, device_id: &str) -> Result<Vec<AppRow>, StoreError> {
        self.query(
            "SELECT device_id, app_id, name, version, fetched_at FROM apps \
             WHERE device_id = ?1 ORDER BY app_id",
            &[s(device_id)],
        )?
        .iter()
        .map(|r| {
            Ok(AppRow {
                device_id: text(r, 0)?,
                app_id: text(r, 1)?,
                name: text(r, 2)?,
                version: text(r, 3)?,
                fetched_at: int(r, 4)?,
            })
        })
        .collect()
    }

    fn delete_app(&self, device_id: &str, app_id: &str) -> Result<(), StoreError> {
        self.exec(
            "DELETE FROM apps WHERE device_id = ?1 AND app_id = ?2",
            &[s(device_id), s(app_id)],
        )
    }

    fn add_capability(&self, device_id: &str, cap: &str) -> Result<(), StoreError> {
        self.exec(
            "INSERT OR REPLACE INTO capabilities (device_id, cap) VALUES (?1, ?2)",
            &[s(device_id), s(cap)],
        )
    }

    fn list_capabilities(&self, device_id: &str) -> Result<Vec<String>, StoreError> {
        self.query(
            "SELECT cap FROM capabilities WHERE device_id = ?1 ORDER BY cap",
            &[s(device_id)],
        )?
        .iter()
        .map(|r| text(r, 0))
        .collect()
    }

    fn remove_capability(&self, device_id: &str, cap: &str) -> Result<(), StoreError> {
        self.exec(
            "DELETE FROM capabilities WHERE device_id = ?1 AND cap = ?2",
            &[s(device_id), s(cap)],
        )
    }

    fn upsert_access_tier(&self, t: &AccessTierRow) -> Result<(), StoreError> {
        self.exec(
            "INSERT OR REPLACE INTO access_tier (device_id, tier, probe_at, probe_map) \
             VALUES (?1,?2,?3,?4)",
            &[s(&t.device_id), s(&t.tier), i(t.probe_at), s(&t.probe_map)],
        )
    }

    fn get_access_tier(&self, device_id: &str) -> Result<Option<AccessTierRow>, StoreError> {
        self.query(
            "SELECT device_id, tier, probe_at, probe_map FROM access_tier WHERE device_id = ?1",
            &[s(device_id)],
        )?
        .first()
        .map(|r| {
            Ok(AccessTierRow {
                device_id: text(r, 0)?,
                tier: text(r, 1)?,
                probe_at: int(r, 2)?,
                probe_map: text(r, 3)?,
            })
        })
        .transpose()
    }

    fn delete_access_tier(&self, device_id: &str) -> Result<(), StoreError> {
        self.exec(
            "DELETE FROM access_tier WHERE device_id = ?1",
            &[s(device_id)],
        )
    }

    fn insert_action(&self, a: &ActionRow) -> Result<i64, StoreError> {
        let rows = self.in_transaction(|conn| {
            conn.query_with_params_sync(
                "INSERT INTO actions (at, client, surface, intent, decision, result, before_state) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7) RETURNING id",
                &[
                    i(a.at),
                    s(&a.client),
                    s(&a.surface),
                    s(&a.intent),
                    s(&a.decision),
                    s(&a.result),
                    s(&a.before_state),
                ],
            )
        })?;
        rows.first()
            .map_or_else(|| Err(backend("insert returned no id")), |row| int(row, 0))
    }

    fn get_action(&self, id: i64) -> Result<Option<ActionRow>, StoreError> {
        self.query(
            &format!("SELECT {ACTION_COLS} FROM actions WHERE id = ?1"),
            &[i(id)],
        )?
        .first()
        .map(action)
        .transpose()
    }

    fn list_actions(&self) -> Result<Vec<ActionRow>, StoreError> {
        self.query(
            &format!("SELECT {ACTION_COLS} FROM actions ORDER BY id"),
            &[],
        )?
        .iter()
        .map(action)
        .collect()
    }

    fn try_record_policy_hit(
        &self,
        client: &str,
        verb: &str,
        since: i64,
        budget: i64,
        now: i64,
    ) -> Result<bool, StoreError> {
        self.in_transaction(|conn| {
            conn.execute_with_params_sync(
                "DELETE FROM policy_hits WHERE at < ?1",
                &[i(now - 3600)],
            )
            .map(drop)?;
            let rows = conn.query_with_params_sync(
                "SELECT at FROM policy_hits WHERE client = ?1 AND verb = ?2 AND at >= ?3",
                &[s(client), s(verb), i(since)],
            )?;
            if rows.len() as i64 >= budget {
                return Ok(false);
            }
            conn.execute_with_params_sync(
                "INSERT INTO policy_hits (client, verb, at) VALUES (?1, ?2, ?3)",
                &[s(client), s(verb), i(now)],
            )
            .map(drop)?;
            Ok(true)
        })
    }

    fn peek_confirmation_token_expiry(&self, token_hash: &str) -> Result<Option<i64>, StoreError> {
        self.query(
            "SELECT expires_at FROM confirmation_tokens WHERE token_hash = ?1",
            &[s(token_hash)],
        )?
        .first()
        .map(|row| int(row, 0))
        .transpose()
    }
    fn peek_confirmation_grant(
        &self,
        token_hash: &str,
    ) -> Result<Option<ConfirmationGrant>, StoreError> {
        let rows = self.query(
            "SELECT client, expires_at FROM confirmation_tokens WHERE token_hash = ?1",
            &[s(token_hash)],
        )?;
        rows.first()
            .map(|row| {
                Ok(ConfirmationGrant {
                    client: row.get(0).and_then(SqliteValue::as_text).map(str::to_owned),
                    expires_at: int(row, 1)?,
                })
            })
            .transpose()
    }
    fn update_action_result(&self, id: i64, result: &str) -> Result<(), StoreError> {
        self.exec(
            "UPDATE actions SET result = ?1 WHERE id = ?2",
            &[s(result), i(id)],
        )
    }

    fn delete_action(&self, id: i64) -> Result<(), StoreError> {
        self.exec("DELETE FROM actions WHERE id = ?1", &[i(id)])
    }

    fn insert_diff(&self, d: &DiffRow) -> Result<i64, StoreError> {
        let rows = self.query(
            "INSERT INTO diff_results (left_device, right_device, surface, field, left_value, \
             right_value, ran_at) VALUES (?1,?2,?3,?4,?5,?6,?7) RETURNING id",
            &[
                s(&d.left_device),
                s(&d.right_device),
                s(&d.surface),
                s(&d.field),
                s(&d.left_value),
                s(&d.right_value),
                i(d.ran_at),
            ],
        )?;
        rows.first()
            .map_or_else(|| Err(backend("insert returned no id")), |row| int(row, 0))
    }

    fn list_diffs(&self) -> Result<Vec<DiffRow>, StoreError> {
        self.query(
            &format!("SELECT {DIFF_COLS} FROM diff_results ORDER BY id"),
            &[],
        )?
        .iter()
        .map(diff)
        .collect()
    }

    fn delete_diff(&self, id: i64) -> Result<(), StoreError> {
        self.exec("DELETE FROM diff_results WHERE id = ?1", &[i(id)])
    }
}
#[cfg(test)]
mod transaction_tests;
