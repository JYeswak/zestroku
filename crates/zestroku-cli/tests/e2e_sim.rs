//! Consolidated §1 e2e walk over real loopback sockets: devices, info,
//! apps, key, launch against one Permissive simulator, with every
//! command/response recorded as JSON lines. No mocks, no LAN.
#![forbid(unsafe_code)]

use std::process::Command;

use zestroku_core::store::{AccessTierRow, DeviceRow, SqliteStore, Store};

/// Redact single-use confirmation tokens before the argv reaches the walk
/// log: the artifact must never dump grants.
fn redacted_argv(args: &[&str]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            out.push("<redacted>".to_string());
            skip_value = false;
        } else if *arg == "--confirm" {
            out.push((*arg).to_string());
            skip_value = true;
        } else {
            out.push((*arg).to_string());
        }
    }
    out
}

fn run(
    db: &std::path::Path,
    args: &[&str],
    log: &mut Vec<serde_json::Value>,
) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .env("ZESTROKU_DIRECT", "1")
        .env("ZESTROKU_TIER", "permissive")
        .args(args)
        .output()
        .expect("run zestroku");
    log.push(serde_json::json!({
        "argv": redacted_argv(args),
        "exit": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
    }));
    output
}

#[test]
fn e2e_workflow_devices_info_apps_key_launch() {
    let sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let target = format!("127.0.0.1:{}", sim.port());
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let store = SqliteStore::open(&db).expect("open store");
    store
        .upsert_device(&DeviceRow {
            id: target.clone(),
            name: "E2E Roku".into(),
            ip: "127.0.0.1".into(),
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
            device_id: target.clone(),
            tier: "permissive".into(),
            probe_at: 1_700_000_000,
            probe_map: "{}".into(),
        })
        .expect("seed tier");
    let device = zestroku_types::Device {
        host: "127.0.0.1".into(),
        port: sim.port(),
        model_name: "E2E Roku".into(),
        model_number: "FIXTURE1".into(),
        software_version: "15.2.4".into(),
        is_tv: false,
        access_tier: zestroku_types::AccessTier::Permissive,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
        .min(i64::MAX as u64) as i64;
    let key_grant = zestroku_core::mutation::issue_approval(
        &store,
        &device,
        &zestroku_proto::keypress_raw("Home"),
        zestroku_core::mutation::ClientPrincipal::Cli,
        now,
    )
    .expect("mint key grant");
    let launch_grant = zestroku_core::mutation::issue_approval(
        &store,
        &device,
        &zestroku_proto::launch(1001),
        zestroku_core::mutation::ClientPrincipal::Cli,
        now,
    )
    .expect("mint launch grant");
    drop(store);

    // Isolated discovery: targets.toml names only the sim endpoint, SSDP off,
    // so no LAN traffic is possible and the observed identity is the sim's.
    let targets = dir.path().join("targets.toml");
    std::fs::write(
        &targets,
        format!("[[target]]\nhost = \"127.0.0.1\"\nport = {}\n", sim.port()),
    )
    .expect("write targets");
    let targets = targets.to_string_lossy().into_owned();

    let mut log = Vec::new();
    let out = run(
        &db,
        &["devices", "--json", "--targets", &targets, "--no-ssdp"],
        &mut log,
    );
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let doc: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .expect("devices stdout JSON");
    let found = doc["devices"]
        .as_array()
        .expect("devices array")
        .iter()
        .find(|d| d["port"] == sim.port())
        .expect("sim endpoint observed");
    assert_eq!(found["host"], "127.0.0.1");
    assert!(found["model_name"].is_string(), "{found}");

    let out = run(&db, &["info", "--json", &target], &mut log);
    assert_eq!(out.status.code(), Some(0));
    let doc: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .expect("info stdout JSON");
    assert!(doc["model_name"].is_string());

    let out = run(&db, &["apps", "--json", &target], &mut log);
    assert_eq!(out.status.code(), Some(0));
    let doc: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .expect("apps stdout JSON");
    assert!(doc.is_array() || doc["apps"].is_array());

    let out = run(&db, &["key", &target, "Home", "--confirm", &key_grant], &mut log);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run(
        &db,
        &["launch", &target, "1001", "--confirm", &launch_grant],
        &mut log,
    );
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(sim.launch_count(), 1);

    // The JSONL walk record: five commands, every exit observed.
    let path = dir.path().join("e2e.jsonl");
    let mut text = String::new();
    for row in &log {
        text.push_str(&serde_json::to_string(row).expect("jsonl row"));
        text.push('\n');
    }
    std::fs::write(&path, &text).expect("write walk log");
    assert_eq!(log.len(), 5);
    for row in &log {
        assert_eq!(row["exit"], 0, "{row}");
    }
    // The walk artifact redacts grants: neither raw token may appear.
    assert!(!text.contains(&key_grant), "grant dumped to walk log");
    assert!(!text.contains(&launch_grant), "grant dumped to walk log");
}
