//! zestroku-tier-discovery-jt8 [b]: `info` prints the observed tier. Real
//! sockets: a zestroku-sim device on loopback, the real binary, `info
//! 127.0.0.1:<port>` (the `host:port` id form `devices` prints), then the
//! recorded tier read back through the store. Planted negative: a Limited
//! sim must print tier=limited, never permissive.
#![forbid(unsafe_code)]

use std::process::Command;

use zestroku_core::store::{SqliteStore, Store};
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

    // The probe is recorded under the canonical host:port key.
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
