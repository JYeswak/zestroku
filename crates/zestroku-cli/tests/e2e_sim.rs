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
    run_with_tier(db, args, log, true)
}

/// Live-lane reads omit the permissive tier override: no test-injected
/// privilege may accompany a real device request.
fn run_live(
    db: &std::path::Path,
    args: &[&str],
    log: &mut Vec<serde_json::Value>,
) -> std::process::Output {
    run_with_tier(db, args, log, false)
}

fn run_with_tier(
    db: &std::path::Path,
    args: &[&str],
    log: &mut Vec<serde_json::Value>,
    tier_override: bool,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zestroku"));
    command
        .env("ZESTROKU_DB", db)
        .env("ZESTROKU_DIRECT", "1")
        .args(args);
    if tier_override {
        command.env("ZESTROKU_TIER", "permissive");
    }
    let output = command.output().expect("run zestroku");
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
    // Live-lane switch (opt-in, never CI): with ZESTROKU_E2E_LIVE=1 and
    // ZESTROKU_E2E_TARGET=host:port, the identical walk runs against a real
    // owner-LAN device. Live mutations remain owner-approved; CI never sets
    let live_target = std::env::var("ZESTROKU_E2E_LIVE")
        .is_ok_and(|v| v == "1")
        .then(|| std::env::var("ZESTROKU_E2E_TARGET").expect("ZESTROKU_E2E_TARGET=host:port"));
    let sim = (!live_target.is_some()).then(|| {
        zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
            kind: zestroku_sim::DeviceKind::Express,
            tier: zestroku_sim::SimTier::Permissive,
        })
    });
    let live = live_target.is_some();
    let (host, port) = match (&live_target, &sim) {
        (Some(target), None) => {
            let (h, p) = target.split_once(':').expect("host:port");
            (h.to_string(), p.parse::<u16>().expect("port"))
        }
        (None, Some(sim)) => ("127.0.0.1".to_string(), sim.port()),
        _ => panic!("ZESTROKU_E2E_LIVE=1 requires exactly one ZESTROKU_E2E_TARGET"),
    };
    let target = format!("{host}:{port}");
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let store = SqliteStore::open(&db).expect("open store");
    store
        .upsert_device(&DeviceRow {
            id: target.clone(),
            name: "E2E Roku".into(),
            ip: host.clone(),
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
    // (Device value constructed inside the sim branch below.)
    // Grant minting is strictly sim-only: a live device must never receive
    // an automatically minted approval (per-op owner approval, repo rule 3).
    // The live lane therefore exercises reads only; mutations stay on sims.
    let (key_grant, launch_grant) = if live {
        drop(store);
        (String::new(), String::new())
    } else {
        let device = zestroku_types::Device {
            host: host.clone(),
            port,
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
        (key_grant, launch_grant)
    };

    // Discovery targets the walk endpoint only (sim port, or the live
    // target), SSDP off, so no other LAN traffic is possible.
    let targets = dir.path().join("targets.toml");
    std::fs::write(
        &targets,
        format!("[[target]]\nhost = \"{host}\"\nport = {port}\n"),
    )
    .expect("write targets");
    let targets = targets.to_string_lossy().into_owned();

    // Reads run on both lanes (live without the tier override); mutations
    // below are sim-only.
    let read: fn(&std::path::Path, &[&str], &mut Vec<serde_json::Value>) -> std::process::Output =
        if live { run_live } else { run };
    let mut log = Vec::new();
    let out = read(
        &db,
        &["devices", "--json", "--targets", &targets, "--no-ssdp"],
        &mut log,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("devices stdout JSON");
    let found = doc["devices"]
        .as_array()
        .expect("devices array")
        .iter()
        .find(|d| d["port"] == port)
        .expect("walk endpoint observed");
    assert_eq!(found["host"], host.as_str());
    assert!(found["model_name"].is_string(), "{found}");

    let out = read(&db, &["info", "--json", &target], &mut log);
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("info stdout JSON");
    assert!(doc["model_name"].is_string());

    let out = read(&db, &["apps", "--json", &target], &mut log);
    assert_eq!(out.status.code(), Some(0));
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("apps stdout JSON");
    assert!(doc.is_array() || doc["apps"].is_array());

    // Live lane stops here: reads only, no minted grants, no mutations.
    if live {
        write_walk_log(&dir, &log, 3);
        return;
    }

    let out = run(
        &db,
        &["key", &target, "Home", "--confirm", &key_grant],
        &mut log,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run(
        &db,
        &["launch", &target, "1001", "--confirm", &launch_grant],
        &mut log,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(sim.as_ref().expect("sim").launch_count(), 1);

    // The JSONL walk record: five commands, every exit observed.
    write_walk_log(&dir, &log, 5);
    let text = std::fs::read_to_string(dir.path().join("e2e.jsonl")).expect("read walk log");
    // The walk artifact redacts grants: neither raw token may appear.
    assert!(!text.contains(&key_grant), "grant dumped to walk log");
    assert!(!text.contains(&launch_grant), "grant dumped to walk log");
}

fn write_walk_log(dir: &tempfile::TempDir, log: &[serde_json::Value], expected: usize) {
    assert_eq!(log.len(), expected);
    for row in log {
        assert_eq!(row["exit"], 0, "{row}");
    }
    let mut text = String::new();
    for row in log {
        text.push_str(&serde_json::to_string(row).expect("jsonl row"));
        text.push('\n');
    }
    std::fs::write(dir.path().join("e2e.jsonl"), &text).expect("write walk log");
}
