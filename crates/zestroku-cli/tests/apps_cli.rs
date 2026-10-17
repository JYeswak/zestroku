#![forbid(unsafe_code)]

// Installed-app name resolution behavior at the CLI boundary.

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
        zestroku_core::mutation::ClientPrincipal::Cli,
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
        .env("ZESTROKU_DIRECT", "1")
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
        .env("ZESTROKU_DIRECT", "1")
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
        .env("ZESTROKU_DIRECT", "1")
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
