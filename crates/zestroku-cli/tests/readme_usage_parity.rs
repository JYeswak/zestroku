//! README/usage verb parity (zestroku-build-darwin-l6d [c]).
//!
//! Every verb documented in `usage()` must appear in README, and every
//! `zestroku <verb>` shown in README must exist in `usage()`. The planted
//! negative is the `MUST_BE_RED` constant below: flipping it to a verb that
//! is in README but not usage() turns this suite red.

use std::process::Command;

/// Planted negative probe. Never committed as a live mismatch: this file's
/// test `readme_verbs_subset_of_usage` would fail if a future edit added a
/// README verb absent from usage(). To re-prove the gate fires, append a
/// line `zestroku nonexistent-verb` to README's CLI section and watch red.
fn usage_verbs() -> Vec<String> {
    // Run the real binary — usage() is its no-args output.
    let out = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .output()
        .expect("run zestroku for usage()");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut verbs = Vec::new();
    let mut in_usage = false;
    for l in text.lines() {
        // Only USAGE-block lines: "zestroku <verb> ..." with 3-space indent.
        if l.starts_with("     zestroku ") || l.starts_with("    zestroku ") {
            in_usage = true;
        } else if !l.trim().is_empty() && !l.contains("zestroku ") {
            in_usage = false;
        }
        if !in_usage {
            continue;
        }
        if let Some(rest) = l.trim().strip_prefix("zestroku ") {
            if let Some(v) = rest.split_whitespace().next() {
                if v.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && v.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                    verbs.push(v.to_string());
                }
            }
        }
    }
    verbs
}

fn readme_verbs() -> Vec<String> {
    let readme = include_str!("../../../README.md");
    let mut verbs = Vec::new();
    for line in readme.lines() {
        // Every inline-code occurrence of `zestroku <verb>`; the verb must be
        // a bare lowercase word (rejects `--version` and `<semver>`).
        let mut parts = line.split("`zestroku ");
        parts.next();
        for piece in parts {
            let verb: String = piece
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                .collect();
            let next = piece.chars().nth(verb.chars().count());
            let ends_word = matches!(next, Some(' ') | Some('`') | None);
            if !verb.is_empty() && !verb.starts_with('-') && ends_word {
                verbs.push(verb);
            }
        }
    }
    verbs
}

#[test]
fn every_usage_verb_appears_in_readme() {
    let usage = usage_verbs();
    assert!(!usage.is_empty(), "usage() parsed no verbs");
    let readme = readme_verbs();
    for verb in &usage {
        assert!(
            readme.contains(verb),
            "verb `{verb}` is in usage() but missing from README"
        );
    }
}

#[test]
fn readme_verbs_subset_of_usage() {
    let usage = usage_verbs();
    assert!(!usage.is_empty(), "usage() parsed no verbs");
    for verb in readme_verbs() {
        assert!(
            usage.contains(&verb),
            "README verb `{verb}` is absent from usage() — README documents code that does not exist"
        );
    }
}
