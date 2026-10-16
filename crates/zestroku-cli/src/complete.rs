//! Shell-completion candidates from local inventory (1r4.2).
//!
//! `zestroku complete devices|apps|verbs [prefix]` prints newline-separated
//! stdout candidates drawn from the store: device ids plus aliases,
//! cached app names/ids per device, and the static verb list. An optional
//! trailing prefix filters. Unknown kinds, bad arity, or an unreadable
//! store exit non-zero on stderr without executing anything; an empty
//! inventory or a cold app cache is empty output, never an error, and an
//! unknown device name never triggers a lookup beyond its own cache rows.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use zestroku_core::store::Store;

use crate::store_db;

/// Verbs the CLI dispatches, offered for shell completion.
pub const VERBS: &[&str] = &[
    "active-app",
    "alias",
    "apps",
    "approve",
    "complete",
    "devices",
    "info",
    "install",
    "key",
    "launch",
    "mcp",
    "serve",
    "version",
];

fn matches_prefix(candidate: &str, prefix: &str) -> bool {
    candidate.starts_with(prefix)
}

fn print_candidates(mut candidates: Vec<String>) -> ExitCode {
    candidates.sort();
    candidates.dedup();
    for candidate in candidates {
        println!("{candidate}");
    }
    ExitCode::SUCCESS
}

fn complete_devices(prefix: &str) -> ExitCode {
    let store = match store_db::open() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("zestroku complete: store unavailable: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut candidates = Vec::new();
    match store.list_devices() {
        Ok(devices) => candidates.extend(devices.into_iter().map(|row| row.id)),
        Err(error) => {
            eprintln!("zestroku complete: cannot list devices: {error}");
            return ExitCode::FAILURE;
        }
    }
    match store.list_aliases() {
        Ok(aliases) => candidates.extend(aliases.into_iter().map(|row| row.alias)),
        Err(error) => {
            eprintln!("zestroku complete: cannot list aliases: {error}");
            return ExitCode::FAILURE;
        }
    }
    print_candidates(
        candidates
            .into_iter()
            .filter(|candidate| matches_prefix(candidate, prefix))
            .collect(),
    )
}

fn complete_apps(device_id: &str, prefix: &str) -> ExitCode {
    let store = match store_db::open() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("zestroku complete: store unavailable: {error}");
            return ExitCode::FAILURE;
        }
    };
    let rows = match store.list_apps(device_id) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("zestroku complete: cannot list cached apps: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut candidates = Vec::with_capacity(rows.len() * 2);
    for row in rows {
        candidates.push(row.id);
        candidates.push(row.name);
    }
    print_candidates(
        candidates
            .into_iter()
            .filter(|candidate| matches_prefix(candidate, prefix))
            .collect(),
    )
}

fn complete_verbs(prefix: &str) -> ExitCode {
    print_candidates(
        VERBS
            .iter()
            .map(|verb| (*verb).to_owned())
            .filter(|verb| matches_prefix(verb, prefix))
            .collect(),
    )
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("zestroku complete: {message}");
    eprintln!("usage: zestroku complete <devices|apps|verbs> [prefix]");
    eprintln!("       zestroku complete apps <device-id> [prefix]");
    ExitCode::from(2)
}

pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("devices") if args.len() <= 2 => {
            complete_devices(args.get(1).map_or("", String::as_str))
        }
        Some("apps") if args.len() == 2 || args.len() == 3 => {
            complete_apps(&args[1], args.get(2).map_or("", String::as_str))
        }
        Some("verbs") if args.len() <= 2 => complete_verbs(args.get(1).map_or("", String::as_str)),
        Some(kind) => usage_error(&format!("unknown completion kind {kind:?}")),
        None => usage_error("missing completion kind"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_filtering_is_exact_prefix_match() {
        assert!(matches_prefix("127.0.0.1:8060", "127"));
        assert!(matches_prefix("127.0.0.1:8060", ""));
        assert!(!matches_prefix("127.0.0.1:8060", "192"));
        assert!(!matches_prefix("living-room", "Living"));
    }

    #[test]
    fn verb_list_covers_dispatched_verbs() {
        for verb in [
            "devices",
            "alias",
            "apps",
            "serve",
            "mcp",
            "info",
            "active-app",
            "approve",
            "install",
            "launch",
            "key",
            "complete",
            "version",
        ] {
            assert!(VERBS.contains(&verb), "verb {verb} missing from completion");
        }
    }
}
