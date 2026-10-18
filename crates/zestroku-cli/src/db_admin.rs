//! `zestroku db backup|restore`: the data half of upgrade and rollback
//! (1r4.9), offline administration of the Store file at `ZESTROKU_DB`.
//!
//! `backup` never migrates: it snapshots the data at the schema it is at, so
//! a newer binary can capture old-schema data before its first `open`
//! migrates it. `restore` replaces the Store with a validated backup; it
//! requires every handle closed (stop the resident daemon through the
//! approved operator route first) and refuses while a writer holds the
//! Store. Neither verb starts, stops or signals any service.
#![forbid(unsafe_code)]

use std::path::Path;
use std::process::ExitCode;

use zestroku_core::store::SqliteStore;
use zestroku_types::ErrorCode;

use crate::store_db;

fn usage_error() -> ExitCode {
    crate::report_cli_error(
        ErrorCode::InvalidArgument,
        "usage: zestroku db backup <dest> | zestroku db restore <backup>",
        &[],
    );
    ExitCode::from(2)
}

pub fn run(args: &[String]) -> ExitCode {
    match args {
        [verb, path] if verb == "backup" => backup(Path::new(path)),
        [verb, path] if verb == "restore" => restore(Path::new(path)),
        _ => usage_error(),
    }
}

fn backup(dest: &Path) -> ExitCode {
    let live = store_db::db_path();
    match SqliteStore::backup_file(&live, dest) {
        Ok(()) => {
            println!("backup: {} <- {}", dest.display(), live.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            crate::report_cli_error(
                ErrorCode::StoreUnavailable,
                format_args!("db backup: {error}"),
                &[],
            );
            ExitCode::FAILURE
        }
    }
}

fn restore(source: &Path) -> ExitCode {
    let live = store_db::db_path();
    match SqliteStore::restore_from(source, &live) {
        Ok(()) => {
            println!("restored: {} <- {}", live.display(), source.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            crate::report_cli_error(
                ErrorCode::StoreUnavailable,
                format_args!("db restore: {error}"),
                &[],
            );
            ExitCode::FAILURE
        }
    }
}
