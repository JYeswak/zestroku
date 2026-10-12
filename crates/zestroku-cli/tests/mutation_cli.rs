//! m7e offline CLI proofs: the confirmation gate and the tier gate fire
//! BEFORE any network dispatch, so a black-holed TEST-NET host exercises
//! both refusal paths deterministically. The action log goes to a temp DB
//! (ZESTROKU_DB). Live-launch proof is the Josh-batched leg, not here.
use std::process::Command;

use zestroku_core::store::Store;

/// Seeds a recorded tier row for a host in the given DB, then runs the key
/// verb against it with a fresh env. Returns (exit, stdout, stderr, tempdir)
/// — the TempDir binding keeps the DB alive for the caller's assertions.
#[allow(clippy::type_complexity)]
fn run_key_with_recorded_tier(
    tier: &str,
    env_extra: &[(&str, &str)],
    extra: &[&str],
) -> (i32, String, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    {
        let store = zestroku_core::store::SqliteStore::open(&db).expect("seed store open");
        store
            .upsert_device(&zestroku_core::store::DeviceRow {
                id: "192.0.2.99".into(),
                name: "seeded".into(),
                ip: "192.0.2.99".into(),
                model: "Sim Express".into(),
                model_num: "SIMX1".into(),
                firmware: "15.2.4".into(),
                wifi_driver: "wifi".into(),
                access_tier: tier.into(),
                last_seen: 1,
            })
            .unwrap();
        store
            .upsert_access_tier(&zestroku_core::store::AccessTierRow {
                device_id: "192.0.2.99".into(),
                tier: tier.into(),
                probe_at: 1,
                probe_map: r#"{"query/apps":"open"}"#.into(),
            })
            .unwrap();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zestroku"));
    cmd.env("ZESTROKU_DB", &db);
    for (k, v) in env_extra {
        cmd.env(k, v);
    }
    cmd.args(["key", "192.0.2.99", "Home"]).args(extra);
    let out = cmd.output().expect("run zestroku key");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).expect("stdout UTF-8"),
        String::from_utf8(out.stderr).expect("stderr UTF-8"),
        dir,
    )
}

#[test]
fn unconfirmed_keypress_refuses_with_audit_exit4() {
    let (code, stdout, stderr, _db) = run_key_with_recorded_tier("permissive", &[], &[]);
    assert_eq!(code, 4, "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("CONFIRMATION_REQUIRED"), "stderr={stderr}");
    assert!(stderr.contains("audit id"), "stderr={stderr}");
    assert!(stdout.is_empty(), "nothing dispatched: {stdout}");
}

#[test]
fn free_text_confirm_is_rejected_and_audited_without_dispatch() {
    let (code, stdout, stderr, dir) =
        run_key_with_recorded_tier("permissive", &[], &["--confirm", "yes"]);
    assert_eq!(code, 4, "stderr={stderr}");
    assert!(stderr.contains("CONFIRMATION_REQUIRED"), "{stderr}");
    assert!(stdout.is_empty(), "no mutation dispatched: {stdout}");
    let store = zestroku_core::store::SqliteStore::open(&dir.path().join("actions.db")).unwrap();
    let rows = store.list_actions().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].decision, "denied");
    assert_eq!(
        rows[0].result,
        zestroku_core::mutation::CONFIRMATION_INVALID
    );
}

#[test]
fn minted_cli_token_authorizes_once_then_replay_is_denied() {
    use zestroku_types::{AccessTier, Device};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    let device = Device {
        host: "192.0.2.99".into(),
        port: 8060,
        model_name: String::new(),
        model_number: String::new(),
        software_version: String::new(),
        is_tv: false,
        access_tier: AccessTier::Permissive,
    };
    let token = {
        let store = zestroku_core::store::SqliteStore::open(&db).unwrap();
        store
            .upsert_device(&zestroku_core::store::DeviceRow {
                id: "192.0.2.99".into(),
                name: "test".into(),
                ip: "192.0.2.99".into(),
                model: "test".into(),
                model_num: "test".into(),
                firmware: "15".into(),
                wifi_driver: "test".into(),
                access_tier: "permissive".into(),
                last_seen: 1,
            })
            .unwrap();
        store
            .upsert_access_tier(&zestroku_core::store::AccessTierRow {
                device_id: "192.0.2.99".into(),
                tier: "permissive".into(),
                probe_at: 1,
                probe_map: r#"{"query/apps":"open"}"#.into(),
            })
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        zestroku_core::mutation::issue_approval(
            &store,
            &device,
            &zestroku_proto::keypress_raw("HOME"),
            now,
        )
        .unwrap()
    };
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_zestroku"))
            .env("ZESTROKU_DB", &db)
            .env("ZESTROKU_TIER", "permissive")
            .args(["key", "192.0.2.99", "Home", "--confirm", &token])
            .output()
            .unwrap()
    };
    let first = invoke();
    assert_eq!(
        first.status.code(),
        Some(5),
        "valid token passes gate; stderr={}",
        String::from_utf8_lossy(&first.stderr)
    );
    let replay = invoke();
    assert_eq!(replay.status.code(), Some(4), "replay must be refused");
    let store = zestroku_core::store::SqliteStore::open(&db).unwrap();
    let rows = store.list_actions().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].decision, "approved");
    assert_eq!(rows[1].decision, "denied");
    assert_eq!(
        rows[1].result,
        zestroku_core::mutation::CONFIRMATION_INVALID
    );
}

#[test]
fn approve_without_terminal_refuses_before_opening_store() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .args(["approve", "launch", "192.0.2.10", "12"])
        .output()
        .expect("run approve");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("interactive terminal"));
    assert!(
        !db.exists(),
        "noninteractive approval must not create a token store"
    );
}

#[test]
fn blank_confirm_token_equals_unconfirmed() {
    let (code, _stdout, stderr, _db) =
        run_key_with_recorded_tier("permissive", &[], &["--confirm", "   "]);
    assert_eq!(code, 4, "stderr={stderr}");
    assert!(stderr.contains("CONFIRMATION_REQUIRED"));
}

// ---- zestroku-tier-discovery-jt8 [c] --------------------------------------

#[test]
fn unknown_tier_mutation_exits_3_names_info_and_writes_no_actions_row() {
    // A device with NO recorded tier: mutation refused before the gate
    // shapes an audit row — and pointed at `zestroku info`.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .args(["launch", "192.0.2.99", "12"])
        .output()
        .expect("run zestroku launch");
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8(out.stderr).expect("stderr UTF-8");
    assert_eq!(code, 3, "stderr={stderr}");
    assert!(stderr.contains("no recorded tier"), "{stderr}");
    assert!(stderr.contains("zestroku info"), "{stderr}");
    // Zero audit rows: the refusal happened before the gate.
    let store = zestroku_core::store::SqliteStore::open(&db).expect("reopen");
    assert!(store.list_actions().unwrap().is_empty());
}

#[test]
fn unknown_tier_with_env_override_still_audited_and_marks_tier_source() {
    // An explicit tier override does not turn arbitrary text into a token.
    // The attempted mutation must be refused and audited.
    // makes this assertion red.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .env("ZESTROKU_TIER", "permissive")
        .args(["key", "192.0.2.99", "Home", "--confirm", "jr"])
        .output()
        .expect("run zestroku key");
    let code = out.status.code().unwrap_or(-1);
    assert_eq!(code, 4, "stderr={}", String::from_utf8_lossy(&out.stderr));
    let store = zestroku_core::store::SqliteStore::open(&db).expect("reopen");
    let rows = store.list_actions().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].decision, "denied");
    assert_eq!(
        rows[0].result,
        zestroku_core::mutation::CONFIRMATION_INVALID
    );
    assert!(
        rows[0]
            .before_state
            .starts_with("tier_source=env-override; "),
        "before_state must carry the tier-source marker: {:?}",
        rows[0].before_state
    );
}

#[test]
fn recorded_limited_tier_refuses_without_any_override() {
    // Recorded tier = limited: the recorded fact gates the mutation (exit 3)
    // even though the live device is Permissive today — the record is what
    // the CLI trusts until re-probed by `zestroku info`.
    let (code, _stdout, stderr, dir) =
        run_key_with_recorded_tier("limited", &[], &["--confirm", "yes"]);
    assert_eq!(code, 3, "stderr={stderr}");
    assert!(stderr.contains("not allowed in Open mode"), "{stderr}");
    let store =
        zestroku_core::store::SqliteStore::open(&dir.path().join("actions.db")).expect("reopen");
    let rows = store.list_actions().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].result, "TIER_REFUSED");
    assert!(
        rows[0].before_state.starts_with("tier_source=recorded; "),
        "{:?}",
        rows[0].before_state
    );
}
#[test]
fn sideload_requires_confirmation_then_authenticates_and_audits_on_installer_port() {
    use std::io::Write;
    use std::process::Stdio;

    use zestroku_core::mutation::issue_approval;
    use zestroku_core::store::{AccessTierRow, DeviceRow, SqliteStore, Store};
    use zestroku_proto::{Command as EcpCommand, Method};
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};
    use zestroku_types::{AccessTier, Device};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let host = "127.0.0.1";
    let installer_port = sim.port();
    let dir = tempfile::tempdir().expect("temporary test directory");
    let db = dir.path().join("actions.db");
    let zip_path = dir.path().join("generated-owner-zip.zip");
    std::fs::write(&zip_path, b"PK\x03\x04simulated owner archive")
        .expect("write simulated ZIP bytes");
    let ecp_id = zestroku_core::tier::device_id(host, 8060);
    {
        let store = SqliteStore::open(&db).expect("open test store");
        store
            .upsert_device(&DeviceRow {
                id: ecp_id.clone(),
                name: "sim".into(),
                ip: host.into(),
                model: "Sim Express".into(),
                model_num: "SIMX1".into(),
                firmware: "15.2.4".into(),
                wifi_driver: "sim".into(),
                access_tier: "permissive".into(),
                last_seen: 1,
            })
            .expect("seed simulated device");
        store
            .upsert_access_tier(&AccessTierRow {
                device_id: ecp_id,
                tier: "permissive".into(),
                probe_at: 1,
                probe_map: r#"{"query/apps":"open"}"#.into(),
            })
            .expect("seed simulated observed tier");
    }

    let mut denied = Command::new(env!("CARGO_BIN_EXE_zestroku"));
    denied
        .env("ZESTROKU_DB", &db)
        .env("ZESTROKU_TIER", "permissive")
        .args(["install", host])
        .arg(&zip_path)
        .args(["--password-stdin", "--installer-port"])
        .arg(installer_port.to_string())
        .stdin(Stdio::piped());
    let mut denied = denied.spawn().expect("start unconfirmed install");
    drop(denied.stdin.take());
    let denied = denied
        .wait_with_output()
        .expect("wait for unconfirmed install");
    assert_eq!(denied.status.code(), Some(4));
    assert!(
        sim.received_requests().is_empty(),
        "confirmation refusal must precede installer HTTP: {:?}",
        sim.received_requests()
    );

    let token = {
        let store = SqliteStore::open(&db).expect("reopen test store");
        issue_approval(
            &store,
            &Device {
                host: host.into(),
                port: installer_port,
                model_name: String::new(),
                model_number: String::new(),
                software_version: String::new(),
                is_tv: false,
                access_tier: AccessTier::Permissive,
            },
            &EcpCommand {
                method: Method::Post,
                path: "/plugin_install".into(),
                tier: AccessTier::Permissive,
            },
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_secs() as i64,
        )
        .expect("mint port-bound installer approval")
    };
    let alternate_port = installer_port.wrapping_add(1).max(1);
    let wrong_port = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .env("ZESTROKU_TIER", "permissive")
        .args(["install", host])
        .arg(&zip_path)
        .args(["--password-stdin", "--installer-port"])
        .arg(alternate_port.to_string())
        .args(["--confirm", &token])
        .stdin(Stdio::piped())
        .output()
        .expect("run installer with wrong token port");
    assert_eq!(wrong_port.status.code(), Some(4));
    assert!(
        sim.received_requests().is_empty(),
        "port-mismatched token must not dispatch"
    );
    let test_password = format!("simulated-{}", std::process::id());
    let mut approved = Command::new(env!("CARGO_BIN_EXE_zestroku"));
    approved
        .env("ZESTROKU_DB", &db)
        .env("ZESTROKU_TIER", "permissive")
        .args(["install", host])
        .arg(&zip_path)
        .args(["--password-stdin", "--installer-port"])
        .arg(installer_port.to_string())
        .args(["--confirm", &token])
        .stdin(Stdio::piped());
    let mut approved = approved.spawn().expect("start approved install");
    approved
        .stdin
        .take()
        .expect("password pipe")
        .write_all(test_password.as_bytes())
        .expect("supply ephemeral test credential");
    let approved = approved
        .wait_with_output()
        .expect("wait for approved install");
    assert_eq!(
        approved.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&approved.stdout),
        String::from_utf8_lossy(&approved.stderr)
    );
    assert_eq!(
        sim.received_requests(),
        vec![
            ("POST".to_string(), "/plugin_install".to_string()),
            ("POST".to_string(), "/plugin_install".to_string()),
        ]
    );
    let store = SqliteStore::open(&db).expect("read audit rows");
    let rows = store.list_actions().expect("list audit rows");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].decision, "denied");
    assert_eq!(rows[1].decision, "denied");
    assert_eq!(rows[2].decision, "approved");
    assert!(rows[2].result.contains("installer reported success"));
    assert!(!rows.iter().any(|row| row.intent.contains(&test_password)));
    assert!(
        !rows
            .iter()
            .any(|row| row.intent.contains("generated-owner-zip.zip"))
    );
}

fn seed_app_store(db: &std::path::Path) {
    use zestroku_core::store::{AccessTierRow, DeviceRow, SqliteStore, Store};

    let ecp_id = zestroku_core::tier::device_id("127.0.0.1", 8060);
    let store = SqliteStore::open(db).expect("open test store");
    store
        .upsert_device(&DeviceRow {
            id: ecp_id.clone(),
            name: "sim".into(),
            ip: "127.0.0.1".into(),
            model: "Sim Express".into(),
            model_num: "SIMX1".into(),
            firmware: "15.2.4".into(),
            wifi_driver: "sim".into(),
            access_tier: "permissive".into(),
            last_seen: 1,
        })
        .expect("seed simulated device");
    store
        .upsert_access_tier(&AccessTierRow {
            device_id: ecp_id,
            tier: "permissive".into(),
            probe_at: 1,
            probe_map: r#"{"query/apps":"open"}"#.into(),
        })
        .expect("seed simulated observed tier");
}

fn mint_launch_token(db: &std::path::Path, app_id: u32, ecp_port: u16) -> String {
    use zestroku_core::mutation::issue_approval;
    use zestroku_core::store::SqliteStore;
    use zestroku_proto::{Command as EcpCommand, Method};
    use zestroku_types::{AccessTier, Device};

    let store = SqliteStore::open(db).expect("reopen test store");
    issue_approval(
        &store,
        &Device {
            host: "127.0.0.1".into(),
            port: ecp_port,
            model_name: String::new(),
            model_number: String::new(),
            software_version: String::new(),
            is_tv: false,
            access_tier: AccessTier::Permissive,
        },
        &EcpCommand {
            method: Method::Post,
            path: format!("/launch/{app_id}"),
            tier: AccessTier::Permissive,
        },
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64,
    )
    .expect("mint launch approval")
}

#[test]
fn launch_app_resolves_unique_name_case_insensitively_and_audits_name_and_id() {
    use std::process::Command;

    use zestroku_core::store::{SqliteStore, Store};
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("temporary test directory");
    let db = dir.path().join("actions.db");
    seed_app_store(&db);
    let token = mint_launch_token(&db, 1001, sim.port());
    let target = format!("127.0.0.1:{}", sim.port());
    // NOTE: the sim serves ECP on its loopback port; the CLI addresses it
    // directly here. Tier comes from the env override, as in the other
    // mutation_cli legs.
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .env("ZESTROKU_TIER", "permissive")
        .args([
            "launch",
            &target,
            "--app",
            "streaming one",
            "--confirm",
            &token,
        ])
        .output()
        .expect("run zestroku launch --app");
    let stdout = String::from_utf8(out.stdout).expect("stdout UTF-8");
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(
        sim.received_requests()
            .contains(&("POST".to_string(), "/launch/1001".to_string())),
        "{:?}",
        sim.received_requests()
    );
    let store = SqliteStore::open(&db).expect("read audit rows");
    let rows = store.list_actions().expect("list audit rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].decision, "approved");
    assert!(
        rows[0].intent.contains("streaming one"),
        "{}",
        rows[0].intent
    );
    assert!(rows[0].intent.contains("1001"), "{}", rows[0].intent);
}

#[test]
fn launch_app_ambiguous_prefix_is_refused_without_posts() {
    use std::process::Command;

    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("temporary test directory");
    let db = dir.path().join("actions.db");
    seed_app_store(&db);
    let target = format!("127.0.0.1:{}", sim.port());
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .env("ZESTROKU_TIER", "permissive")
        .args(["launch", &target, "--app", "streaming"])
        .output()
        .expect("run zestroku launch --app ambiguous");
    let stderr = String::from_utf8(out.stderr).expect("stderr UTF-8");
    assert_ne!(out.status.code(), Some(0));
    assert!(stderr.contains("AMBIGUOUS_APP"), "{stderr}");
    assert!(
        sim.received_requests()
            .iter()
            .all(|(method, _)| method != "POST"),
        "{:?}",
        sim.received_requests()
    );
}

#[test]
fn launch_app_absent_name_is_refused_without_posts() {
    use std::process::Command;

    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("temporary test directory");
    let db = dir.path().join("actions.db");
    seed_app_store(&db);
    let target = format!("127.0.0.1:{}", sim.port());
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &db)
        .env("ZESTROKU_TIER", "permissive")
        .args(["launch", &target, "--app", "no such app"])
        .output()
        .expect("run zestroku launch --app absent");
    let stderr = String::from_utf8(out.stderr).expect("stderr UTF-8");
    assert_ne!(out.status.code(), Some(0));
    assert!(stderr.contains("APP_NOT_INSTALLED"), "{stderr}");
    assert!(
        sim.received_requests()
            .iter()
            .all(|(method, _)| method != "POST"),
        "{:?}",
        sim.received_requests()
    );
}
