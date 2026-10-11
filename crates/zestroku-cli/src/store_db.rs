//! Shared store-path handling for the CLI (action log + tier/inventory DB).
//! One small module so `mutate.rs`, `main.rs`, and tests agree on the
//! location: `$ZESTROKU_DB` or `~/.local/state/zestroku/actions.db`.
#![forbid(unsafe_code)]

use std::path::PathBuf;

use zestroku_core::store::SqliteStore;

/// The resolved DB path (exported for tests and the usage text).
pub fn db_path() -> PathBuf {
    std::env::var_os("ZESTROKU_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()));
            p.push(".local/state/zestroku/actions.db");
            p
        })
}

/// Opens the store, creating the parent directory when missing.
pub fn open() -> Result<SqliteStore, String> {
    let path = db_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    SqliteStore::open(&path).map_err(|e| e.to_string())
}
