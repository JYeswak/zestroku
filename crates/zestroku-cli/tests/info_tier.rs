//! zestroku-tier-discovery-jt8 [b]: `info` prints the observed tier. Real
//! sockets: a zestroku-sim device on loopback, the real binary, and the
//! canonical `host:port` endpoint recorded by `devices`.
#![forbid(unsafe_code)]

use std::process::Command;

use zestroku_core::store::{DeviceRow, SqliteStore, Store};
use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

fn info(db: &std::path::Path, target: &str) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .args(["info", target])
        .output()
        .expect("run zestroku info");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).expect("stdout UTF-8"),
        String::from_utf8(out.stderr).expect("stderr UTF-8"),
    )
}

fn run_cli(db: &std::path::Path, args: &[&str], permissive: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zestroku"));
    command.env("ZESTROKU_DB", db).args(args);
    if permissive {
        command.env("ZESTROKU_TIER", "permissive");
    }
    command.output().expect("run zestroku command")
}

fn seed_device(store: &SqliteStore, id: &str, name: &str, model: &str, model_num: &str) {
    let ip = id.rsplit_once(':').map_or(id, |(host, _)| host);
    store
        .upsert_device(&DeviceRow {
            id: id.into(),
            name: name.into(),
            ip: ip.into(),
            model: model.into(),
            model_num: model_num.into(),
            firmware: "15.2.4".into(),
            wifi_driver: String::new(),
            access_tier: "permissive".into(),
            last_seen: 1,
        })
        .unwrap();
}

fn check(kind: DeviceKind, tier: SimTier, want_tier: &str, want_tv_channel: &str) {
    let sim = SimDevice::start(SimConfig { kind, tier });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    let id = format!("127.0.0.1:{}", sim.port());

    let (code, stdout, stderr) = info(&db, &id);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains(&format!("tier={want_tier}")),
        "want tier={want_tier}: {stdout}"
    );
    assert!(
        stdout.contains(&format!("tv-active-channel={want_tv_channel}")),
        "want tv-active-channel={want_tv_channel}: {stdout}"
    );

    let store = SqliteStore::open(&db).expect("open store");
    let row = store
        .get_access_tier(&id)
        .expect("read access_tier")
        .unwrap_or_else(|| panic!("no access_tier row for {id}"));
    assert_eq!(row.tier, want_tier);
}

#[test]
fn info_prints_and_records_permissive_on_the_express() {
    check(
        DeviceKind::Express,
        SimTier::Permissive,
        "permissive",
        "absent",
    );
}

#[test]
fn info_prints_and_records_limited_on_a_limited_device() {
    check(DeviceKind::Express, SimTier::Limited, "limited", "absent");
}

#[test]
fn info_reports_tv_channel_on_the_tv() {
    check(DeviceKind::Tv, SimTier::Permissive, "permissive", "present");
}

#[test]
fn alias_set_list_remove_and_info_resolution_round_trip() {
    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("aliases.db");
    let id = format!("127.0.0.1:{}", sim.port());
    let store = SqliteStore::open(&db).expect("open inventory store");
    seed_device(&store, &id, "Living Room Roku", "Roku Express", "3941X2");
    drop(store);

    let set = run_cli(&db, &["alias", "set", "living-room", &id], false);
    assert_eq!(
        set.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    let listed = run_cli(&db, &["alias", "ls"], false);
    assert_eq!(listed.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&listed.stdout).contains(&format!("living-room -> {id}")));

    let (code, stdout, stderr) = info(&db, "living-room");
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("tier=permissive"), "{stdout}");

    let removed = run_cli(&db, &["alias", "rm", "living-room"], false);
    assert_eq!(
        removed.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let listed = run_cli(&db, &["alias", "ls"], false);
    assert!(String::from_utf8_lossy(&listed.stdout).contains("no aliases"));
}

#[test]
fn ambiguous_tv_refuses_reads_and_mutations_before_network_requests() {
    let tv_a = SimDevice::start(SimConfig {
        kind: DeviceKind::Tv,
        tier: SimTier::Permissive,
    });
    let tv_b = SimDevice::start(SimConfig {
        kind: DeviceKind::Tv,
        tier: SimTier::Permissive,
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("ambiguous.db");
    let id_a = format!("127.0.0.1:{}", tv_a.port());
    let id_b = format!("127.0.0.1:{}", tv_b.port());
    let store = SqliteStore::open(&db).expect("seed inventory");
    seed_device(&store, &id_a, "Bedroom TV", "Roku TV", "L816X");
    seed_device(&store, &id_b, "Living TV", "Roku TV", "L815X");
    drop(store);
    let requests_a = tv_a.received_requests();
    let requests_b = tv_b.received_requests();

    let info_out = run_cli(&db, &["info", "tv"], false);
    let info_err = String::from_utf8_lossy(&info_out.stderr);
    assert_eq!(info_out.status.code(), Some(2), "{info_err}");
    assert!(info_err.contains("AMBIGUOUS_NAME"), "{info_err}");
    assert!(
        info_err.contains(&id_a) && info_err.contains(&id_b),
        "{info_err}"
    );
    assert_eq!(tv_a.received_requests(), requests_a);
    assert_eq!(tv_b.received_requests(), requests_b);

    let mutation = run_cli(&db, &["key", "tv", "Home", "--confirm", "untrusted"], true);
    let mutation_err = String::from_utf8_lossy(&mutation.stderr);
    assert_eq!(mutation.status.code(), Some(4), "{mutation_err}");
    assert!(mutation_err.contains("AMBIGUOUS_NAME"), "{mutation_err}");
    assert_eq!(tv_a.received_requests(), requests_a);
    assert_eq!(tv_b.received_requests(), requests_b);
    let store = SqliteStore::open(&db).expect("reopen alias audit store");
    let denied = store.list_actions().unwrap();
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].decision, "denied");
    assert_eq!(denied[0].result, "AMBIGUOUS_NAME");
}
