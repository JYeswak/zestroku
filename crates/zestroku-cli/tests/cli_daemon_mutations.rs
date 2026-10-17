#![forbid(unsafe_code)]

//! The daemon owns mutation approvals and audit rows; the CLI is only a client.

#[path = "common/mutation_daemon.rs"]
mod mutation_daemon;

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use mutation_daemon::{http_free_port, mint_http_approval, seed_http_device, start_http_daemon};
use zestroku_core::store::{SqliteStore, Store};
use zestroku_proto::{keypress_raw, launch};
use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

fn run_cli(client_db: &Path, daemon_url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", client_db)
        .env("ZESTROKU_DAEMON_URL", daemon_url)
        .env_remove("ZESTROKU_DIRECT")
        .env_remove("ZESTROKU_TIER")
        .env_remove("ZESTROKU_TRANSPORT")
        .args(args)
        .output()
        .expect("run real CLI mutation client")
}

#[test]
fn cli_mutations_use_daemon_authority_and_completed_audit() {
    let mut sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("isolated mutation stores");
    let daemon_db = directory.path().join("daemon.db");
    // Deliberately no client-side device, tier, or approval: only the daemon
    // knows this target and can authorize the opaque tokens below.
    let client_db = directory.path().join("client.db");
    let target = seed_http_device(&daemon_db, sim.port());
    let api_port = http_free_port();
    let daemon_url = format!("http://127.0.0.1:{api_port}");
    let _daemon = start_http_daemon(&daemon_db, api_port);
    let launch_token = mint_http_approval(&daemon_db, sim.port(), &launch(1001));
    let raw_token = mint_http_approval(&daemon_db, sim.port(), &keypress_raw("hOmE"));
    let key_token = mint_http_approval(&daemon_db, sim.port(), &keypress_raw("Home"));

    let launched = run_cli(
        &client_db,
        &daemon_url,
        &["launch", &target, "1001", "--confirm", &launch_token],
    );
    assert_eq!(
        launched.status.code(),
        Some(0),
        "resident launch must not require a client-side tier or token store: {}",
        String::from_utf8_lossy(&launched.stderr)
    );
    assert_eq!(sim.launch_count(), 1);

    let raw_keyed = run_cli(
        &client_db,
        &daemon_url,
        &["key", &target, "hOmE", "--confirm", &raw_token],
    );
    assert_eq!(raw_keyed.status.code(), Some(0), "{raw_keyed:?}");
    let still_active = run_cli(&client_db, &daemon_url, &["active-app", &target, "--json"]);
    assert_eq!(still_active.status.code(), Some(0), "{still_active:?}");
    assert!(still_active.stderr.is_empty(), "{still_active:?}");
    let active_state: serde_json::Value = serde_json::from_slice(&still_active.stdout).unwrap();
    assert_eq!(
        active_state["app_id"], 1001,
        "the raw mixed-case key must not be normalized to Home"
    );
    assert_eq!(active_state["name"], "Streaming One");

    let keyed = run_cli(
        &client_db,
        &daemon_url,
        &["key", &target, "Home", "--confirm", &key_token],
    );
    assert_eq!(keyed.status.code(), Some(0), "{keyed:?}");
    let active = run_cli(&client_db, &daemon_url, &["active-app", &target, "--json"]);
    assert_eq!(active.status.code(), Some(0), "{active:?}");
    assert!(active.stderr.is_empty(), "{active:?}");
    let home: serde_json::Value = serde_json::from_slice(&active.stdout).unwrap();
    assert_eq!(home["name"], "Roku");
    assert!(home["app_id"].is_null());

    // A different empty read-client store cannot supply or mask mutation authority.
    let read_client_db = directory.path().join("read-client.db");
    let info = run_cli(&read_client_db, &daemon_url, &["info", &target, "--json"]);
    assert_eq!(info.status.code(), Some(0), "{info:?}");
    assert!(info.stderr.is_empty(), "{info:?}");
    let device: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    assert_eq!(device["model_name"], "Sim Express");
    assert_eq!(device["model_number"], "SIMX1");
    assert_eq!(device["is_tv"], false);
    assert_eq!(device["access_tier"], "permissive");

    for args in [
        vec![
            "launch",
            target.as_str(),
            "1001",
            "--confirm",
            launch_token.as_str(),
        ],
        vec!["key", target.as_str(), "Home", "--confirm", "yes"],
        vec!["key", target.as_str(), "Home", "--confirm", "   "],
        vec!["key", target.as_str(), "Home"],
    ] {
        let denied = run_cli(&client_db, &daemon_url, &args);
        assert_eq!(denied.status.code(), Some(4), "{denied:?}");
        assert!(String::from_utf8_lossy(&denied.stderr).contains("CONFIRMATION_REQUIRED"));
        assert!(
            denied.stdout.is_empty(),
            "a refusal must not report success"
        );
    }

    let failed_token = mint_http_approval(&daemon_db, sim.port(), &launch(1002));
    sim.suspend();
    let failed = run_cli(
        &client_db,
        &daemon_url,
        &["launch", &target, "1002", "--confirm", &failed_token],
    );
    assert_eq!(failed.status.code(), Some(5), "{failed:?}");
    assert!(
        failed.stdout.is_empty(),
        "wire failure is not completed success"
    );
    let posts: Vec<_> = sim
        .received_requests()
        .into_iter()
        .filter_map(|(method, path)| (method == "POST").then_some(path))
        .collect();
    assert_eq!(posts, ["/launch/1001", "/keypress/hOmE", "/keypress/Home"]);
    assert_eq!(sim.launch_count(), 1, "denial and failure must not retry");

    let actions = SqliteStore::open(&daemon_db)
        .expect("open daemon audit")
        .list_actions()
        .expect("read daemon actions");
    assert_eq!(actions.len(), 8);
    for (action, (before_app_id, before)) in actions[..3].iter().zip([
        (None, "Roku"),
        (Some(1001), "Streaming One"),
        (Some(1001), "Streaming One"),
    ]) {
        assert_eq!(action.client, "http");
        assert_eq!(action.decision, "approved");
        assert_eq!(action.result, "completed");
        assert_eq!(
            action.before_state,
            zestroku_core::tier::marked_before_app_state(
                "recorded",
                &target,
                before_app_id,
                before
            )
        );
    }
    assert_eq!(actions[0].surface, "launch");
    assert_eq!(actions[1].surface, "key");
    assert_eq!(actions[2].surface, "key");
    assert!(
        actions[3..7]
            .iter()
            .all(|action| action.decision == "denied")
    );
    assert_eq!(actions[7].decision, "approved");
    assert_eq!(actions[7].result, "dispatch_failed");
    assert_eq!(
        actions[7].before_state,
        zestroku_core::tier::marked_before_app_state("recorded", &target, None, "unreachable")
    );
    assert!(
        SqliteStore::open(&client_db)
            .expect("open client audit")
            .list_actions()
            .expect("read client actions")
            .is_empty(),
        "default CLI must not authorize or audit locally"
    );
    for secret in [&launch_token, &raw_token, &key_token, &failed_token] {
        for output in [&launched, &raw_keyed, &keyed, &failed] {
            assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
            assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
        }
        assert!(actions.iter().all(|action| !action.intent.contains(secret)));
    }
}

#[test]
fn cli_devices_reads_daemon_cache_without_discovery_or_fallback() {
    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("isolated inventory stores");
    let daemon_db = directory.path().join("daemon.db");
    let client_db = directory.path().join("client.db");
    let target = seed_http_device(&daemon_db, sim.port());
    {
        let store = SqliteStore::open(&daemon_db).expect("open daemon inventory");
        let mut row = store.get_device(&target).unwrap().unwrap();
        row.model_num = "CACHED_MODEL_ONLY".into();
        store.upsert_device(&row).expect("save cached metadata");
    }
    let api_port = http_free_port();
    let daemon_url = format!("http://127.0.0.1:{api_port}");
    let daemon = start_http_daemon(&daemon_db, api_port);
    let output = run_cli(&client_db, &daemon_url, &["devices", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["source"], "daemon");
    assert_eq!(view["devices"][0]["host"], "127.0.0.1");
    assert_eq!(view["devices"][0]["model_number"], "CACHED_MODEL_ONLY");
    assert_eq!(view["devices"][0]["access_tier"], "permissive");
    assert!(
        sim.received_requests().is_empty(),
        "listing cached inventory must not probe ECP"
    );
    daemon.stop();
    let started = Instant::now();
    let down = run_cli(&client_db, &daemon_url, &["devices", "--json"]);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(down.status.code(), Some(1));
    assert!(down.stdout.is_empty());
    assert!(String::from_utf8_lossy(&down.stderr).contains("DAEMON_UNAVAILABLE"));
    assert!(
        sim.received_requests().is_empty(),
        "a down daemon must not trigger direct discovery"
    );
}

#[test]
fn cli_apps_uses_daemon_tier_with_an_empty_client_database() {
    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("isolated app inventory stores");
    let daemon_db = directory.path().join("daemon.db");
    let client_db = directory.path().join("client.db");
    let target = seed_http_device(&daemon_db, sim.port());
    let api_port = http_free_port();
    let daemon_url = format!("http://127.0.0.1:{api_port}");
    let daemon = start_http_daemon(&daemon_db, api_port);
    for mode in [vec![], vec!["--daemon", daemon_url.as_str()]] {
        let mut args = vec!["apps", target.as_str(), "--json"];
        args.extend(mode);
        let output = run_cli(&client_db, &daemon_url, &args);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            view["apps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|app| { app["id"] == "1001" && app["name"] == "Streaming One" })
        );
    }
    let requests = sim.received_requests();
    daemon.stop();
    let down = run_cli(
        &client_db,
        &daemon_url,
        &["apps", target.as_str(), "--json"],
    );
    assert_eq!(down.status.code(), Some(1), "{down:?}");
    assert!(down.stdout.is_empty());
    assert!(String::from_utf8_lossy(&down.stderr).contains("DAEMON_UNAVAILABLE"));
    assert_eq!(sim.received_requests(), requests, "no direct ECP fallback");
}

#[test]
fn cli_app_name_launch_preserves_requested_name_in_daemon_audit() {
    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("isolated app-name stores");
    let daemon_db = directory.path().join("daemon.db");
    let client_db = directory.path().join("client.db");
    let target = seed_http_device(&daemon_db, sim.port());
    let api_port = http_free_port();
    let daemon_url = format!("http://127.0.0.1:{api_port}");
    let _daemon = start_http_daemon(&daemon_db, api_port);
    let token = mint_http_approval(&daemon_db, sim.port(), &launch(1001));
    let private_name = format!("label-{token}");
    for with_confirmation in [false, true] {
        let mut args = vec!["launch", target.as_str(), "--app", private_name.as_str()];
        if with_confirmation {
            args.extend(["--confirm", token.as_str()]);
        }
        let refused = run_cli(&client_db, &daemon_url, &args);
        assert_eq!(refused.status.code(), Some(2));
        assert!(refused.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(stderr.contains("APP_NOT_INSTALLED"));
        assert!(
            !stderr.contains(&token),
            "app resolution errors must not echo a token pasted into the requested name"
        );
    }
    assert_eq!(sim.launch_count(), 0);
    assert!(
        SqliteStore::open(&daemon_db)
            .unwrap()
            .list_actions()
            .unwrap()
            .is_empty(),
        "name refusal must not consume or audit the approval"
    );
    let output = run_cli(
        &client_db,
        &daemon_url,
        &[
            "launch",
            &target,
            "--app",
            "streaming one",
            "--confirm",
            &token,
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(sim.launch_count(), 1);
    for (name, error_code) in [
        ("streaming", "AMBIGUOUS_APP"),
        ("no such app", "APP_NOT_INSTALLED"),
    ] {
        let refused = run_cli(&client_db, &daemon_url, &["launch", &target, "--app", name]);
        assert_eq!(refused.status.code(), Some(2));
        assert!(refused.stdout.is_empty());
        assert!(String::from_utf8_lossy(&refused.stderr).contains(error_code));
    }
    assert_eq!(sim.launch_count(), 1, "name refusal must never dispatch");
    let actions = SqliteStore::open(&daemon_db)
        .expect("open daemon audit")
        .list_actions()
        .expect("read app-name action");
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].decision, "approved");
    assert_eq!(actions[0].result, "completed");
    assert_eq!(
        actions[0].before_state,
        zestroku_core::tier::marked_before_app_state("recorded", &target, None, "Roku")
    );
    assert!(
        actions[0]
            .intent
            .contains("requested_app=\"streaming one\"")
    );
    assert!(actions[0].intent.contains("resolved_app_id=1001"));
    assert!(actions[0].intent.contains("/launch/1001"));
    assert!(!actions[0].intent.contains(&token));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&token));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&token));
}

#[test]
fn cli_daemon_down_preserves_approval_for_restart() {
    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("isolated restart stores");
    let daemon_db = directory.path().join("daemon.db");
    let client_db = directory.path().join("client.db");
    let target = seed_http_device(&daemon_db, sim.port());
    let api_port = http_free_port();
    let daemon_url = format!("http://127.0.0.1:{api_port}");
    let token = mint_http_approval(&daemon_db, sim.port(), &launch(1001));
    let daemon = start_http_daemon(&daemon_db, api_port);
    daemon.stop();

    let started = Instant::now();
    let unavailable = run_cli(
        &client_db,
        &daemon_url,
        &["launch", &target, "1001", "--confirm", &token],
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(unavailable.status.code(), Some(5), "{unavailable:?}");
    assert!(String::from_utf8_lossy(&unavailable.stderr).contains("DAEMON_UNAVAILABLE"));
    assert!(unavailable.stdout.is_empty());
    assert!(
        sim.received_requests().is_empty(),
        "daemon-down must not fall back to LAN"
    );
    assert!(
        SqliteStore::open(&daemon_db)
            .expect("open daemon store while stopped")
            .list_actions()
            .expect("read stopped daemon audit")
            .is_empty()
    );

    let _restarted = start_http_daemon(&daemon_db, api_port);
    let completed = run_cli(
        &client_db,
        &daemon_url,
        &["launch", &target, "1001", "--confirm", &token],
    );
    assert_eq!(
        completed.status.code(),
        Some(0),
        "same token must survive: {completed:?}"
    );
    assert_eq!(sim.launch_count(), 1);
    let actions = SqliteStore::open(&daemon_db)
        .expect("open restarted daemon audit")
        .list_actions()
        .expect("read restarted audit");
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].decision, "approved");
    assert_eq!(actions[0].result, "completed");
    assert!(
        SqliteStore::open(&client_db)
            .expect("open client store after restart")
            .list_actions()
            .expect("read client audit after restart")
            .is_empty()
    );
}

#[test]
fn cli_log_reads_action_rows_without_device_requests() {
    use zestroku_core::store::{ActionRow, Store};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("isolated action log store");
    let db = directory.path().join("actions.db");
    let store = SqliteStore::open(&db).expect("open action log store");
    for (at, surface, decision, result) in [
        (10, "launch", "approved", "completed"),
        (11, "undo", "denied", "CONFIRMATION_REQUIRED"),
    ] {
        store
            .insert_action(&ActionRow {
                id: 0,
                at,
                client: "http".into(),
                surface: surface.into(),
                intent: format!("{surface} 127.0.0.1:{} /launch/1001", sim.port()),
                decision: decision.into(),
                result: result.into(),
                before_state: "tier_source=recorded; Roku".into(),
            })
            .expect("insert action log row");
    }
    drop(store);

    let output = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .args(["log", "--json"])
        .output()
        .expect("run read-only log command");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON action log");
    let actions = view["actions"].as_array().expect("actions array");
    assert_eq!(actions.len(), 2, "{view}");
    assert_eq!(actions[0]["surface"], "undo");
    assert_eq!(actions[0]["decision"], "denied");
    assert_eq!(actions[0]["result"], "CONFIRMATION_REQUIRED");
    assert_eq!(actions[1]["surface"], "launch");
    assert_eq!(actions[1]["client"], "http");
    assert_eq!(
        actions[1]["intent"],
        format!("launch 127.0.0.1:{} /launch/1001", sim.port())
    );
    assert_eq!(actions[1]["before_state"], "tier_source=recorded; Roku");
    let human = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .arg("log")
        .output()
        .expect("run human action log");
    assert_eq!(human.status.code(), Some(0), "{human:?}");
    let table = String::from_utf8_lossy(&human.stdout);
    for field in [
        "client=http",
        "surface=undo",
        "decision=denied",
        "result=\"CONFIRMATION_REQUIRED\"",
    ] {
        assert!(table.contains(field), "{table}");
    }
    assert!(
        table.contains(&format!(
            "intent=\"launch 127.0.0.1:{} /launch/1001\"",
            sim.port()
        )),
        "{table}"
    );
    assert!(
        sim.received_requests().is_empty(),
        "reading the action log must not contact the device"
    );
}

#[test]
fn cli_undo_requires_confirmation_and_logs_the_restored_app() {
    let mut sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("isolated undo stores");
    let db = directory.path().join("actions.db");
    let target = seed_http_device(&db, sim.port());
    let api_port = http_free_port();
    let daemon_url = format!("http://127.0.0.1:{api_port}");
    let _daemon = start_http_daemon(&db, api_port);

    for app_id in [1001, 1002] {
        let token = mint_http_approval(&db, sim.port(), &launch(app_id));
        let output = run_cli(
            &db,
            &daemon_url,
            &["launch", &target, &app_id.to_string(), "--confirm", &token],
        );
        assert_eq!(output.status.code(), Some(0), "{output:?}");
    }

    let undo_token = mint_http_approval(&db, sim.port(), &launch(1001));
    let denied = run_cli(&db, &daemon_url, &["undo", &target]);
    assert_eq!(denied.status.code(), Some(4), "{denied:?}");
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("CONFIRMATION_REQUIRED"),
        "{denied:?}"
    );
    assert_eq!(sim.launch_count(), 2, "denied undo must not dispatch");

    let restored = run_cli(
        &db,
        &daemon_url,
        &["undo", &target, "--confirm", &undo_token],
    );
    assert_eq!(restored.status.code(), Some(0), "{restored:?}");
    assert_eq!(sim.launch_count(), 3);
    assert_eq!(
        sim.received_requests()
            .into_iter()
            .filter(|(method, _)| method == "POST")
            .map(|(_, path)| path)
            .collect::<Vec<_>>(),
        ["/launch/1001", "/launch/1002", "/launch/1001"]
    );

    let actions = SqliteStore::open(&db)
        .expect("open undo audit")
        .list_actions()
        .expect("read undo audit");
    assert_eq!(actions[2].surface, "undo");
    assert_eq!(actions[2].decision, "denied");
    assert_eq!(actions[2].result, "CONFIRMATION_REQUIRED");
    assert_eq!(actions[3].surface, "undo");
    assert_eq!(actions[3].decision, "approved");
    assert_eq!(actions[3].result, "completed");
    assert!(
        actions[3].intent.contains("Streaming One"),
        "{:?}",
        actions[3]
    );
    assert!(
        actions[3].before_state.contains("Streaming Two"),
        "{:?}",
        actions[3]
    );
}

fn assert_undo_refuses_unrestorable_snapshot(before_state: impl FnOnce(&str) -> String) {
    use zestroku_core::store::{ActionRow, Store};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("isolated undo store");
    let db = directory.path().join("actions.db");
    let target = seed_http_device(&db, sim.port());
    SqliteStore::open(&db)
        .expect("open action store")
        .insert_action(&ActionRow {
            id: 0,
            at: 10,
            client: "http".into(),
            surface: "launch".into(),
            intent: format!("launch {target} /launch/1001"),
            decision: "approved".into(),
            result: "completed".into(),
            before_state: before_state(&target),
        })
        .expect("seed prior action");
    let api_port = http_free_port();
    let daemon_url = format!("http://127.0.0.1:{api_port}");
    let _daemon = start_http_daemon(&db, api_port);

    let output = run_cli(&db, &daemon_url, &["undo", &target]);
    assert_ne!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("could not restore"),
        "{output:?}"
    );
    assert!(
        sim.received_requests()
            .iter()
            .all(|(method, _)| method != "POST"),
        "an unverifiable previous app identity must never be launched"
    );
}

#[test]
fn cli_undo_refuses_legacy_name_only_snapshot() {
    assert_undo_refuses_unrestorable_snapshot(|_| "tier_source=recorded; Streaming One".to_owned());
}

#[test]
fn cli_undo_refuses_uninstalled_prior_id_even_when_name_matches() {
    assert_undo_refuses_unrestorable_snapshot(|target| {
        format!("tier_source=recorded; device_id={target}; app_id=9999; app_name=Streaming One")
    });
}
