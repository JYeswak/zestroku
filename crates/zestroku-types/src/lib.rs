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

/// Stable error-code registry (bead zestroku-m8-error-codes-d2u).
/// Codes are additive and never renamed: wire snapshots pin the strings,
/// and renaming any code turns the snapshot test red. The same code renders
/// in CLI output, HTTP JSON bodies, and MCP error payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    UnknownDevice,
    AmbiguousName,
    AmbiguousApp,
    DeviceUnreachable,
    DeviceSuspended,
    TierRefused,
    ConfirmationRequired,
    PolicyDenied,
    AppNotInstalled,
    StoreUnavailable,
    LanBlockedByOsPolicy,
    DaemonUnavailable,
}

impl ErrorCode {
    /// The wire-stable code string. Never change an existing arm.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownDevice => "UNKNOWN_DEVICE",
            Self::AmbiguousName => "AMBIGUOUS_NAME",
            Self::AmbiguousApp => "AMBIGUOUS_APP",
            Self::DeviceUnreachable => "DEVICE_UNREACHABLE",
            Self::DeviceSuspended => "DEVICE_SUSPENDED",
            Self::TierRefused => "TIER_REFUSED",
            Self::ConfirmationRequired => "CONFIRMATION_REQUIRED",
            Self::PolicyDenied => "POLICY_DENIED",
            Self::AppNotInstalled => "APP_NOT_INSTALLED",
            Self::StoreUnavailable => "STORE_UNAVAILABLE",
            Self::LanBlockedByOsPolicy => "LAN_BLOCKED_BY_OS_POLICY",
            Self::DaemonUnavailable => "DAEMON_UNAVAILABLE",
        }
    }

    /// Every registry code in declaration order (snapshot test).
    pub const fn all() -> [Self; 12] {
        [
            Self::UnknownDevice,
            Self::AmbiguousName,
            Self::AmbiguousApp,
            Self::DeviceUnreachable,
            Self::DeviceSuspended,
            Self::TierRefused,
            Self::ConfirmationRequired,
            Self::PolicyDenied,
            Self::AppNotInstalled,
            Self::StoreUnavailable,
            Self::LanBlockedByOsPolicy,
            Self::DaemonUnavailable,
        ]
    }
}

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

    /// Additive-only snapshot: renaming any code (e.g. TIER_REFUSED) or
    /// reordering the registry turns this red. New codes append at the end.
    #[test]
    fn error_codes_snapshot() {
        let codes: Vec<&str> = ErrorCode::all().iter().map(|c| c.as_str()).collect();
        assert_eq!(
            codes,
            [
                "UNKNOWN_DEVICE",
                "AMBIGUOUS_NAME",
                "AMBIGUOUS_APP",
                "DEVICE_UNREACHABLE",
                "DEVICE_SUSPENDED",
                "TIER_REFUSED",
                "CONFIRMATION_REQUIRED",
                "POLICY_DENIED",
                "APP_NOT_INSTALLED",
                "STORE_UNAVAILABLE",
                "LAN_BLOCKED_BY_OS_POLICY",
                "DAEMON_UNAVAILABLE",
            ]
        );
    }
}
