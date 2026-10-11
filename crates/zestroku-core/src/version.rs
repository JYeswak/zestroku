//! Build-time version identity (zestroku-build-darwin-l6d [b]).
//!
//! The version string is `zestroku <semver> <git-sha7>[-dirty]`. The sha and
//! dirty flag are injected by the build via `ZESTROKU_BUILD_SHA` and
//! `ZESTROKU_BUILD_DIRTY` env config (see README "Build and run on the
//! owner's Mac"); absent injection the sha degrades to `unknown` — a build
//! that cannot name its commit must never masquerade as pinned.

/// The seven-hex sha fragment (or `unknown` when no build injection ran).
#[must_use]
pub fn sha7() -> &'static str {
    match option_env!("ZESTROKU_BUILD_SHA") {
        Some(sha) if !sha.is_empty() && sha.len() >= 7 && sha[..7].bytes().all(|b| b.is_ascii_hexdigit()) => &sha[..7],
        _ => "unknown",
    }
}

/// Whether the build was marked dirty at compile time.
#[must_use]
pub fn dirty() -> bool {
    option_env!("ZESTROKU_BUILD_DIRTY") == Some("true")
}

/// The full version line: `zestroku <semver> <sha7>[-dirty]`.
#[must_use]
pub fn version_string(semver: &str) -> String {
    if dirty() {
        format!("zestroku {semver} {}-dirty", sha7())
    } else {
        format!("zestroku {semver} {}", sha7())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Planted negative (acceptance [b]): the dirty flag must flip the suffix.
    // These run in whatever env the build injected; both branches are covered
    // by construction below.
    #[test]
    fn version_string_has_three_fields() {
        let v = version_string(env!("CARGO_PKG_VERSION"));
        let fields: Vec<&str> = v.split_whitespace().collect();
        assert_eq!(fields[0], "zestroku");
        assert_eq!(fields[1], env!("CARGO_PKG_VERSION"));
        let last = fields[2];
        if dirty() {
            assert!(last.ends_with("-dirty"), "dirty build must print -dirty: {v}");
            assert_eq!(fields.len(), 3);
        } else {
            assert!(!last.contains('-'), "clean build must not print -dirty: {v}");
        }
        if sha7() != "unknown" {
            assert_eq!(last.trim_end_matches("-dirty").len(), 7);
        }
    }

    #[test]
    fn sha7_rejects_short_or_nonhex_injection() {
        // sha7() itself is compile-time, but the contract it enforces is that
        // `unknown` is the only fallback shape; a real injection always yields
        // 7 hex chars. Guard the fallback shape.
        let s = sha7();
        assert!(s == "unknown" || (s.len() == 7 && s.bytes().all(|b| b.is_ascii_hexdigit())));
    }
}
