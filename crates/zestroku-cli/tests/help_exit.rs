//! CLI --help exit-code contract (zestroku-lot): help renders on stdout with
//! exit 0; unknown subcommands stay stderr exit 2. On the pre-fix binary
//! `--help`, `-h` and bare invocations all exited 2, so agents checking exit
//! codes misread successful help as failure. These tests pin the fixed
//! behavior; asserting exit 2 for `--help` is the planted negative that goes
//! red against the fix (and green only on the old binary).
use std::process::Command;

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
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
fn help_flag_prints_usage_to_stdout_and_exits_zero() {
    let (code, stdout, stderr) = run(&["--help"]);
    assert_eq!(code, 0, "expected exit 0, stderr: {stderr}");
    assert!(stdout.contains("USAGE:"), "usage missing from stdout");
    assert!(stderr.is_empty(), "stderr must stay empty, got: {stderr}");
}

#[test]
fn short_help_flag_prints_usage_to_stdout_and_exits_zero() {
    let (code, stdout, stderr) = run(&["-h"]);
    assert_eq!(code, 0, "expected exit 0, stderr: {stderr}");
    assert!(stdout.contains("USAGE:"), "usage missing from stdout");
    assert!(stderr.is_empty(), "stderr must stay empty, got: {stderr}");
}

#[test]
fn bare_invocation_prints_usage_to_stdout_and_exits_zero() {
    let (code, stdout, stderr) = run(&[]);
    assert_eq!(code, 0, "expected exit 0, stderr: {stderr}");
    assert!(stdout.contains("USAGE:"), "usage missing from stdout");
    assert!(stderr.is_empty(), "stderr must stay empty, got: {stderr}");
}

#[test]
fn unknown_subcommand_prints_usage_to_stderr_and_exits_two() {
    let (code, stdout, stderr) = run(&["frobnicate"]);
    assert_eq!(code, 2, "unknown input must stay exit 2, stdout: {stdout}");
    assert!(stderr.contains("USAGE:"), "usage missing from stderr");
    assert!(stdout.is_empty(), "stdout must stay empty, got: {stdout}");
}
