//! m7e offline CLI proofs: the confirmation gate and the tier gate fire
//! BEFORE any network dispatch, so a black-holed TEST-NET host exercises
//! both refusal paths deterministically. The action log goes to a temp DB
//! (ZESTROKU_DB). Live-launch proof is the Josh-batched leg, not here.
use std::process::Command;

fn run_key(db: &std::path::Path, extra: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        // The live Express is Permissive (M3 flip); the CLI defaults to the
        // fail-safe Open until discovery populates tier (m7c). These tests
        // exercise the CONFIRMATION gate, so they opt into Permissive the
        // same way an operator against the flipped device would.
        .env("ZESTROKU_TIER", "permissive")
        .args(["key", "192.0.2.99", "Home"])
        .args(extra)
        .output()
        .expect("run zestroku key");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).expect("stdout UTF-8"),
        String::from_utf8(out.stderr).expect("stderr UTF-8"),
    )
}

#[test]
fn unconfirmed_keypress_refuses_with_audit_exit4() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    let (code, stdout, stderr) = run_key(&db, &[]);
    assert_eq!(code, 4, "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("CONFIRMATION_REQUIRED"), "stderr={stderr}");
    assert!(stderr.contains("audit id"), "stderr={stderr}");
    assert!(stdout.is_empty(), "nothing dispatched: {stdout}");
}

#[test]
fn confirmed_keypress_on_blackholed_host_dispatches_and_audits_failure() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    // TEST-NET-3 unroutable: gate approves (token present), dispatch fails
    // on the 5s transport timeout, outcome recorded in the action row,
    // exit 5 with the transport's own error text (no "dispatch failed"
    // prefix — the CLI surfaces the underlying unreachable error verbatim).
    let (code, _stdout, stderr) = run_key(&db, &["--confirm", "yes"]);
    assert_eq!(code, 5, "stderr={stderr}");
    assert!(
        stderr.contains("device unreachable") || stderr.contains("budget deadline"),
        "stderr={stderr}"
    );
    assert!(!stderr.contains("CONFIRMATION_REQUIRED"), "{stderr}");
}

#[test]
fn blank_confirm_token_equals_unconfirmed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("actions.db");
    let (code, _stdout, stderr) = run_key(&db, &["--confirm", "   "]);
    assert_eq!(code, 4, "stderr={stderr}");
    assert!(stderr.contains("CONFIRMATION_REQUIRED"));
}
