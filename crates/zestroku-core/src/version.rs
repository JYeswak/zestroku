//! Build-time version identity (zestroku-build-darwin-l6d [b]).
//!
//! The version string is `zestroku <semver> <git-sha7>[-dirty]`. The sha and
//! dirty flag are injected by the build via `ZESTROKU_BUILD_SHA` and
//! `ZESTROKU_BUILD_DIRTY` env config (see README "Build and run on the
//! owner's Mac"); absent injection the sha degrades to `unknown` — a build
//! that cannot name its commit must never masquerade as pinned.

/// The seven-hex fragment of an injected sha, or `unknown` when the value is
/// absent, shorter than seven characters, or not hex there.
fn sha7_of(raw: Option<&str>) -> &str {
    match raw {
        Some(sha)
            if sha.len() >= 7
                && sha.is_char_boundary(7)
                && sha[..7].bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            &sha[..7]
        }
        _ => "unknown",
    }
}

/// The version line from explicit parts (the pure core of [`version_string`]).
fn render(semver: &str, sha7: &str, dirty: bool) -> String {
    if dirty {
        format!("zestroku {semver} {sha7}-dirty")
    } else {
        format!("zestroku {semver} {sha7}")
    }
}

/// The seven-hex sha fragment (or `unknown` when no build injection ran).
#[must_use]
pub fn sha7() -> &'static str {
    sha7_of(option_env!("ZESTROKU_BUILD_SHA"))
}

/// Whether the build was marked dirty at compile time.
#[must_use]
pub fn dirty() -> bool {
    option_env!("ZESTROKU_BUILD_DIRTY") == Some("true")
}

/// The full version line: `zestroku <semver> <sha7>[-dirty]`.
#[must_use]
pub fn version_string(semver: &str) -> String {
    render(semver, sha7(), dirty())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Acceptance [b] planted negatives, both branches forced (not whatever
    // the test build happened to inject).
    #[test]
    fn dirty_flag_adds_the_suffix_and_clean_does_not() {
        assert_eq!(
            render("0.1.0", "abc1234", true),
            "zestroku 0.1.0 abc1234-dirty"
        );
        assert_eq!(render("0.1.0", "abc1234", false), "zestroku 0.1.0 abc1234");
    }

    #[test]
    fn sha7_takes_seven_hex_and_refuses_everything_else() {
        let full = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(sha7_of(Some(full)), "0123456");
        assert_eq!(sha7_of(Some("ABCDEF1234")), "ABCDEF1");
        assert_eq!(sha7_of(None), "unknown");
        assert_eq!(sha7_of(Some("")), "unknown");
        assert_eq!(sha7_of(Some("abc123")), "unknown", "six chars");
        assert_eq!(sha7_of(Some("zzzzzzz1")), "unknown", "not hex");
        assert_eq!(sha7_of(Some("\u{e9}\u{e9}\u{e9}\u{e9}1234")), "unknown");
    }

    #[test]
    fn build_injected_version_is_well_formed() {
        let v = version_string(env!("CARGO_PKG_VERSION"));
        let fields: Vec<&str> = v.split_whitespace().collect();
        assert_eq!(fields.len(), 3, "{v}");
        assert_eq!(fields[0], "zestroku");
        assert_eq!(fields[1], env!("CARGO_PKG_VERSION"));
        let sha = fields[2].trim_end_matches("-dirty");
        assert!(sha == "unknown" || sha.len() == 7, "{v}");
        assert_eq!(fields[2].ends_with("-dirty"), dirty(), "{v}");
    }
}
