//! 1r4.2 usability: completion candidates from a seeded store and explicit
//! read-mode flags, exercised through the real binary. No network: every
//! case either reads the local store or fails before any I/O (usage errors,
//! refused daemon connections).
#![forbid(unsafe_code)]

use std::process::Command;

use zestroku_core::store::{AccessTierRow, AliasRow, AppRow, DeviceRow, SqliteStore, Store};

fn seed_store(db: &std::path::Path) {
    let store = SqliteStore::open(db).expect("open store");
    store
        .upsert_device(&DeviceRow {
            id: "192.0.2.10:8060".into(),
            name: "Fixture Roku".into(),
            ip: "192.0.2.10".into(),
            model: "Roku Fixture".into(),
            model_num: "FIXTURE1".into(),
            firmware: "15.2.4".into(),
            wifi_driver: "fixture".into(),
            access_tier: "permissive".into(),
            last_seen: 1_700_000_000,
        })
        .expect("seed device");
    store
        .upsert_access_tier(&AccessTierRow {
            device_id: "192.0.2.10:8060".into(),
            tier: "permissive".into(),
            probe_at: 1_700_000_000,
            probe_map: "{}".into(),
        })
        .expect("seed recorded tier");
    store
        .set_alias(&AliasRow {
            alias: "living-room".into(),
            device_id: "192.0.2.10:8060".into(),
        })
        .expect("seed alias");
    store
        .upsert_app(&AppRow {
            device_id: "192.0.2.10:8060".into(),
            app_id: "1001".into(),
            name: "Streaming One".into(),
            version: "1.0".into(),
            fetched_at: 1_700_000_000,
        })
        .expect("seed app");
}

fn run(db: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .env_remove("ZESTROKU_DIRECT")
        .env_remove("ZESTROKU_DAEMON_URL")
        .args(args)
        .output()
        .expect("run zestroku")
}

fn stdout_lines(output: &std::process::Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn complete_devices_lists_ids_and_aliases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let output = run(&db, &["complete", "devices"]);
    assert_eq!(output.status.code(), Some(0));
    let lines = stdout_lines(&output);
    assert!(lines.contains(&"192.0.2.10:8060".to_owned()), "{lines:?}");
    assert!(lines.contains(&"living-room".to_owned()), "{lines:?}");
}

#[test]
fn complete_devices_prefix_filters_without_executing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let output = run(&db, &["complete", "devices", "living"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_lines(&output), vec!["living-room".to_owned()]);
}

#[test]
fn complete_apps_uses_the_refreshed_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let output = run(&db, &["complete", "apps", "192.0.2.10:8060"]);
    assert_eq!(output.status.code(), Some(0));
    let lines = stdout_lines(&output);
    assert!(lines.contains(&"Streaming One".to_owned()), "{lines:?}");
    assert!(lines.contains(&"1001".to_owned()), "{lines:?}");
}

#[test]
fn complete_apps_unknown_device_is_empty_not_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let output = run(&db, &["complete", "apps", "192.0.2.99:8060"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout_lines(&output).is_empty());
}

#[test]
fn complete_verbs_lists_dispatched_verbs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let output = run(&db, &["complete", "verbs", "in"]);
    assert_eq!(output.status.code(), Some(0));
    let lines = stdout_lines(&output);
    assert!(lines.contains(&"info".to_owned()), "{lines:?}");
    assert!(lines.contains(&"install".to_owned()), "{lines:?}");
    assert!(!lines.contains(&"devices".to_owned()), "{lines:?}");
}

#[test]
fn complete_unknown_kind_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let output = run(&db, &["complete", "widgets"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout_lines(&output).is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn complete_apps_resolves_alias_to_canonical_device_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let output = run(&db, &["complete", "apps", "living-room"]);
    assert_eq!(output.status.code(), Some(0));
    let lines = stdout_lines(&output);
    assert!(lines.contains(&"Streaming One".to_owned()), "{lines:?}");
}

#[test]
fn read_mode_flag_conflict_fails_before_any_io() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    for verb in ["info", "apps"] {
        let output = run(
            &db,
            &[
                verb,
                "--daemon",
                "http://127.0.0.1:9",
                "--direct",
                "192.0.2.10:8060",
            ],
        );
        assert_eq!(output.status.code(), Some(2), "{verb}");
        assert!(stdout_lines(&output).is_empty(), "{verb}");
    }
}

#[test]
fn explicit_daemon_down_refuses_without_fallback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let dead = "http://127.0.0.1:9";
    let output = run(&db, &["info", "--daemon", dead, "192.0.2.10:8060"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(output.status.code(), Some(0));
    assert!(stderr.contains("DAEMON_UNAVAILABLE"), "{stderr}");
}

#[test]
fn complete_script_bash_registers_and_delegates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let output = run(&db, &["complete", "--script", "bash"]);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).expect("script UTF-8");
    assert!(text.contains("complete -F _zestroku zestroku"), "{text}");
    assert!(text.contains("zestroku complete verbs"), "{text}");
    assert!(text.contains("zestroku complete devices"), "{text}");
    assert!(output.stderr.is_empty());
}

#[test]
fn complete_script_unknown_shell_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let output = run(&db, &["complete", "--script", "fish"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout_lines(&output).is_empty());
}

#[test]
fn bash_adapter_sources_and_offers_inventory_candidates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let script = dir.path().join("zestroku.bash");
    let output = run(&db, &["complete", "--script", "bash"]);
    assert_eq!(output.status.code(), Some(0));
    std::fs::write(&script, &output.stdout).expect("write adapter");
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_zestroku"));
    let bin_dir = binary.parent().expect("binary directory");
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    let probe = Command::new("bash")
        .env("ZESTROKU_DB", &db)
        .env("PATH", &path)
        .arg("-c")
        .arg("source \"$0\"; COMP_WORDS=(zestroku complete devices); COMP_CWORD=2; _zestroku; printf '%s\\n' \"${COMPREPLY[@]}\"")
        .arg(script)
        .output()
        .expect("run bash probe");
    assert_eq!(probe.status.code(), Some(0), "{probe:?}");
    let offered = String::from_utf8_lossy(&probe.stdout);
    assert!(offered.contains("192.0.2.10:8060"), "{offered}");
    assert!(offered.contains("living-room"), "{offered}");
}
