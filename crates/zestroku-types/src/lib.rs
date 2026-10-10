//! zestroku-types — pure domain vocabulary. No I/O.
//!
//! Ground truth: skill-library-growth/candidates/edge-library/roku/
//! (COMPREHENSIVE_PLAN_FOR_ZESTROKU.md, AGENTS.md lane doctrine).
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Which ECP command tier a command needs. The core refuses out-of-tier
/// commands by construction (bead zestroku-m3: Limited vs Permissive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessTier {
    /// Works in Restricted ("Limited") mode: read-only introspection.
    Open,
    /// Requires Permissive mode: mutation or enumeration surfaces.
    Permissive,
}

/// One Roku device on the LAN.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    /// IP or hostname reachable on the owner's LAN.
    pub host: String,
    /// ECP port (documented default 8060).
    pub port: u16,
    pub model_name: String,
    pub model_number: String,
    /// `<VER>-<BUILD>`, e.g. "15.2.4".
    pub software_version: String,
    pub is_tv: bool,
    /// Observed mode at discovery time; refreshed by the core.
    pub access_tier: AccessTier,
}

/// A pressed key on the virtual remote (Permissive tier).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key {
    Home,
    Rev,
    Fwd,
    Select,
    Back,
    InstantReplay,
    Info,
    Backspace,
    Search,
    Enter,
}

/// Errors shared across crates.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("device unreachable: {0}")]
    Unreachable(String),
    #[error("command {cmd} not allowed in {mode:?} mode (403)")]
    TierRefused { cmd: String, mode: AccessTier },
    #[error("malformed ECP response: {0}")]
    Malformed(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_refused_formats() {
        let e = Error::TierRefused {
            cmd: "query/apps".into(),
            mode: AccessTier::Open,
        };
        assert!(e.to_string().contains("403"));
    }
}
