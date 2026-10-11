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
fn confirmed_keypress_on_blackholed_host_dispatches_and_audits_failure() {
    // TEST-NET-3 unroutable: gate approves (token present), dispatch fails
    // on the 5s transport timeout, outcome recorded in the action row,
    // exit 5 with the transport's own error text (no "dispatch failed"
    // prefix — the CLI surfaces the underlying unreachable error verbatim).
    let (code, _stdout, stderr, _db) =
        run_key_with_recorded_tier("permissive", &[], &["--confirm", "yes"]);
    assert_eq!(code, 5, "stderr={stderr}");
    assert!(
        stderr.contains("device unreachable") || stderr.contains("budget deadline"),
        "stderr={stderr}"
    );
    assert!(!stderr.contains("CONFIRMATION_REQUIRED"), "{stderr}");
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
    // ZESTROKU_TIER=permissive on an unknown-tier device: allowed (explicit
    // override), but the audit row MUST carry tier_source=env-override.
    // Planted negative [c]: removing the tier_source marker from mutate.rs
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
    // Dispatch to a black-holed host still runs (override granted); the
    // audit row is what this test is about.
    assert_eq!(code, 5, "stderr={}", String::from_utf8_lossy(&out.stderr));
    let store = zestroku_core::store::SqliteStore::open(&db).expect("reopen");
    let rows = store.list_actions().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].decision, "approved");
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
    let store = zestroku_core::store::SqliteStore::open(&dir.path().join("actions.db"))
        .expect("reopen");
    let rows = store.list_actions().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].result, "TIER_REFUSED");
    assert!(
        rows[0].before_state.starts_with("tier_source=recorded; "),
        "{:?}",
        rows[0].before_state
    );
}
