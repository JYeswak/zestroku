//! 1r4.2 usability: completion candidates from a seeded store and explicit
//! read-mode flags, exercised through the real binary. Loopback only: every
//! case either reads the local store, fails before any I/O (usage errors,
//! refused daemon connections), or talks solely to in-process loopback
//! simulators that never leave the host.
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

fn run_env(db: &std::path::Path, args: &[&str], extra: &[(&str, &str)]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zestroku"));
    command
        .env("ZESTROKU_DB", db)
        .env_remove("ZESTROKU_DIRECT")
        .env_remove("ZESTROKU_DAEMON_URL");
    for (key, value) in extra {
        command.env(key, value);
    }
    command.args(args).output().expect("run zestroku")
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
fn complete_devices_ambiguous_prefix_lists_both_without_choosing() {
    // Boundary: a prefix matching two inventory entries lists both and
    // exits 0 with empty stderr — completion never resolves an implicit
    // single choice and never executes.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let store = SqliteStore::open(&db).expect("open store");
    store
        .upsert_device(&DeviceRow {
            id: "192.0.2.11:8060".into(),
            name: "Fixture Roku Two".into(),
            ip: "192.0.2.11".into(),
            model: "Roku Fixture".into(),
            model_num: "FIXTURE2".into(),
            firmware: "15.2.4".into(),
            wifi_driver: "fixture".into(),
            access_tier: "permissive".into(),
            last_seen: 1_700_000_000,
        })
        .expect("seed second device");
    store
        .set_alias(&AliasRow {
            alias: "living-room-2".into(),
            device_id: "192.0.2.11:8060".into(),
        })
        .expect("seed second alias");
    let output = run(&db, &["complete", "devices", "living-room"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let lines = stdout_lines(&output);
    assert!(lines.contains(&"living-room".to_owned()), "{lines:?}");
    assert!(lines.contains(&"living-room-2".to_owned()), "{lines:?}");
}
#[test]
fn complete_apps_ambiguous_device_operand_refuses_to_guess() {
    // Boundary: with two devices holding distinct app caches, an exact
    // alias still resolves to its own canonical cache, while a prefix
    // matching two aliases resolves nothing — empty output, exit 0, no
    // implicit device choice and no execution.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let store = SqliteStore::open(&db).expect("open store");
    store
        .upsert_device(&DeviceRow {
            id: "192.0.2.11:8060".into(),
            name: "Fixture Roku Two".into(),
            ip: "192.0.2.11".into(),
            model: "Roku Fixture".into(),
            model_num: "FIXTURE2".into(),
            firmware: "15.2.4".into(),
            wifi_driver: "fixture".into(),
            access_tier: "permissive".into(),
            last_seen: 1_700_000_000,
        })
        .expect("seed second device");
    store
        .set_alias(&AliasRow {
            alias: "living-room-2".into(),
            device_id: "192.0.2.11:8060".into(),
        })
        .expect("seed second alias");
    store
        .upsert_app(&AppRow {
            device_id: "192.0.2.11:8060".into(),
            app_id: "1002".into(),
            name: "Streaming Two".into(),
            version: "1.0".into(),
            fetched_at: 1_700_000_000,
        })
        .expect("seed second cache");
    let output = run(&db, &["complete", "apps", "living-room"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let lines = stdout_lines(&output);
    assert!(lines.contains(&"Streaming One".to_owned()), "{lines:?}");
    assert!(!lines.contains(&"Streaming Two".to_owned()), "{lines:?}");
    let output = run(&db, &["complete", "apps", "living"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(stdout_lines(&output).is_empty());
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
    // (words, cword, exact expected COMPREPLY entries, in order)
    let contexts: [(&str, u32, Vec<&str>); 6] = [
        ("zestroku complete devices", 2, vec!["devices"]),
        (
            "zestroku complete apps \"\"",
            3,
            vec!["192.0.2.10:8060", "living-room"],
        ),
        ("zestroku complete apps living", 3, vec!["living-room"]),
        (
            "zestroku complete apps living-room \"\"",
            4,
            vec!["1001", "Streaming One"],
        ),
        (
            "zestroku complete apps living-room Streaming",
            4,
            vec!["Streaming One"],
        ),
        (
            "zestroku",
            1,
            vec![
                "active-app",
                "alias",
                "approve",
                "apps",
                "complete",
                "devices",
                "info",
                "install",
                "key",
                "launch",
                "mcp",
                "serve",
                "version",
            ],
        ),
    ];
    for (words, cword, expected) in contexts {
        let probe = Command::new("bash")
            .env("ZESTROKU_DB", &db)
            .env("PATH", &path)
            .arg("-c")
            .arg(format!(
                "source \"$0\"; COMP_WORDS=({words}); COMP_CWORD={cword}; _zestroku; printf '%s\\n' \"${{COMPREPLY[@]}}\""
            ))
            .arg(&script)
            .output()
            .expect("run bash probe");
        assert_eq!(probe.status.code(), Some(0), "{words} {probe:?}");
        let offered: Vec<String> = String::from_utf8_lossy(&probe.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        let expected: Vec<String> = expected.into_iter().map(str::to_owned).collect();
        assert_eq!(offered, expected, "{words}");
    }
}

#[test]
fn explicit_direct_flag_routes_loopback_past_a_dead_daemon() {
    let sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let target = format!("127.0.0.1:{}", sim.port());
    let output = run_env(
        &db,
        &["info", "--direct", &target],
        &[("ZESTROKU_DAEMON_URL", "http://127.0.0.1:9")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Sim Express"), "{stdout}");
    assert!(stdout.contains("SIMX1"), "{stdout}");
}

#[test]
fn direct_json_renders_typed_document_from_loopback_device() {
    let sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_store(&db);
    let target = format!("127.0.0.1:{}", sim.port());
    let output = run_env(
        &db,
        &["info", "--direct", "--json", &target],
        &[("ZESTROKU_DAEMON_URL", "http://127.0.0.1:9")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let document: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("stdout JSON");
    assert!(document["model_name"].is_string());
    assert!(document["model_number"].is_string());
    assert!(document["access_tier"].is_string());
}

#[test]
fn ambiguous_mutation_audit_never_persists_the_confirmation_secret() {
    // Two TV-class loopback simulators make `tv` ambiguous. A REAL
    // single-use grant minted for one of them must deny with AMBIGUOUS_NAME
    // while the raw token appears nowhere persisted or printed, both sims
    // observe zero requests, and the grant survives for reuse on the
    // resolved endpoint (exit 0, exactly one POST, then replay refusal).
    use sha2::{Digest, Sha256};
    use zestroku_core::mutation::{ClientPrincipal, issue_approval};
    use zestroku_proto::keypress_raw;
    use zestroku_types::{AccessTier, Device};

    let sim_a = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let sim_b = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let id_a = format!("127.0.0.1:{}", sim_a.port());
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let store = SqliteStore::open(&db).expect("open store");
    for (id, port) in [
        (&id_a, sim_a.port()),
        (&format!("127.0.0.1:{}", sim_b.port()), sim_b.port()),
    ] {
        let port = port.to_string();
        store
            .upsert_device(&DeviceRow {
                id: id.into(),
                name: "Fixture Roku TV".into(),
                ip: "127.0.0.1".into(),
                model: "Roku TV".into(),
                model_num: format!("L{port}"),
                firmware: "15.2.4".into(),
                wifi_driver: "fixture".into(),
                access_tier: "permissive".into(),
                last_seen: 1_700_000_000,
            })
            .expect("seed tv device");
    }
    let device = Device {
        host: "127.0.0.1".into(),
        port: sim_a.port(),
        model_name: "Fixture Roku TV".into(),
        model_number: format!("L{}", sim_a.port()),
        software_version: "15.2.4".into(),
        is_tv: true,
        access_tier: AccessTier::Permissive,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
        .min(i64::MAX as u64) as i64;
    let grant = issue_approval(
        &store,
        &device,
        &keypress_raw("Home"),
        ClientPrincipal::Cli,
        now,
    )
    .expect("mint real grant");
    let hash = Sha256::digest(grant.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    drop(store);

    // Closed-port daemon URL: an AMBIGUOUS denial proves the denial precedes
    // any dispatch (dispatch first would surface DAEMON_UNAVAILABLE), and
    // both simulators must observe zero requests.
    let output = run_env(
        &db,
        &["key", "tv", "Home", "--confirm", &grant],
        &[("ZESTROKU_DAEMON_URL", "http://127.0.0.1:1")],
    );
    assert_eq!(output.status.code(), Some(4));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains(&grant), "token leaked to stdout");
    assert!(stderr.contains("AMBIGUOUS_NAME"), "{stderr}");
    assert!(!stderr.contains(&grant), "token leaked to stderr");
    assert!(sim_a.received_requests().is_empty(), "ECP left the host");
    assert!(sim_b.received_requests().is_empty(), "ECP left the host");
    let store = SqliteStore::open(&db).expect("reopen store");
    let actions = store.list_actions().expect("list actions");
    let denied: Vec<_> = actions
        .iter()
        .filter(|action| action.result == "AMBIGUOUS_NAME" && action.decision == "denied")
        .collect();
    assert_eq!(denied.len(), 1, "{actions:?}");
    assert!(
        !denied[0].intent.contains(&grant),
        "token persisted: {}",
        denied[0].intent
    );
    assert!(
        denied[0].intent.contains("candidates=["),
        "{}",
        denied[0].intent
    );
    // The grant survives the ambiguous denial: still peekable, nothing approved.
    assert!(
        store
            .peek_confirmation_grant(&hash)
            .expect("peek grant")
            .is_some(),
        "real grant consumed by the ambiguous denial"
    );
    assert!(
        actions.iter().all(|action| action.decision != "approved"),
        "{actions:?}"
    );
    drop(store);

    // Same grant on the resolved endpoint: exit 0 with exactly one POST of
    // the granted command, then replay refusal with the consumed grant.
    let output = run_env(
        &db,
        &["key", &id_a, "Home", "--confirm", &grant],
        &[("ZESTROKU_DIRECT", "1"), ("ZESTROKU_TIER", "permissive")],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let posts: Vec<_> = sim_a
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .collect();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(posts[0].1.ends_with("/keypress/Home"), "{posts:?}");

    // Replaying the consumed grant refuses: consumption proof, since peek
    // cannot distinguish consumed rows (its query has no consumed filter).
    let output = run_env(
        &db,
        &["key", &id_a, "Home", "--confirm", &grant],
        &[("ZESTROKU_DIRECT", "1"), ("ZESTROKU_TIER", "permissive")],
    );
    assert_eq!(
        output.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("CONFIRMATION_REQUIRED"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Replay dispatched nothing further: still exactly the one granted POST.
    let posts: Vec<_> = sim_a
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .collect();
    assert_eq!(posts.len(), 1, "{posts:?}");
}
