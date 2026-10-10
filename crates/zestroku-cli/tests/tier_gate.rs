//! Tier-gate regression core (m7e extension): POST verbs must be refused
//! locally for Open-tier devices — BEFORE the confirmation gate — and the
//! refusal must be audited (TIER_REFUSED actions row via the m7e gate).
//! Encodes wire-doc finding: the gate is a wrapper, not a filter; local
//! refusal means no 403 round-trip ever happens.
use std::process::Command;

fn run(args: &[&str], db: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", db)
        .args(args)
        .output()
        .expect("run zestroku command");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).expect("stdout UTF-8"),
        String::from_utf8(out.stderr).expect("stderr UTF-8"),
    )
}

#[test]
fn keypress_is_refused_before_network_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _stdout, stderr) = run(&["key", "127.0.0.1", "Home"], &dir.path().join("a.db"));
    assert_eq!(code, 3);
    assert!(stderr.contains("not allowed in Open mode"), "{stderr}");
}

#[test]
fn launch_is_refused_for_open_tier_even_with_confirm() {
    // The tier gate runs BEFORE confirmation: a token cannot buy a mutation
    // past the tier wall. This pins ordering regression m7e guards.
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(
        &["launch", "127.0.0.1", "12", "--confirm", "yes"],
        &dir.path().join("a.db"),
    );
    assert_eq!(code, 3, "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("not allowed in Open mode"), "{stderr}");
    assert!(stderr.contains("denial audited"), "{stderr}");
    assert!(stdout.is_empty(), "nothing dispatched: {stdout}");
}

#[test]
fn unconfirmed_launch_on_open_tier_reports_tier_first() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _stdout, stderr) = run(&["launch", "127.0.0.1", "12"], &dir.path().join("a.db"));
    // Tier wall fires first: exit 3 (tier), not 4 (confirmation).
    assert_eq!(code, 3);
    assert!(stderr.contains("not allowed in Open mode"), "{stderr}");
    assert!(!stderr.contains("CONFIRMATION_REQUIRED"), "{stderr}");
}

#[test]
fn get_commands_still_dispatch_without_any_gate() {
    // Sanity anchor: reads remain Open-tier and ungated. 127.0.0.1:8060 has
    // nothing listening -> the CLI reports an unreachable device (exit 1),
    // NOT a tier refusal and NOT a confirmation refusal.
    let dir = tempfile::tempdir().unwrap();
    let (code, _stdout, stderr) = run(&["active-app", "127.0.0.1"], &dir.path().join("a.db"));
    assert_ne!(code, 3, "reads must not hit the tier wall: {stderr}");
    assert_ne!(
        code, 4,
        "reads must not hit the confirmation gate: {stderr}"
    );
}
