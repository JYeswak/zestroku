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
    DoctorWarn,
    DoctorFail,
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
            Self::DoctorWarn => "DOCTOR_WARN",
            Self::DoctorFail => "DOCTOR_FAIL",
        }
    }

    /// One-line owner remedy shown alongside the code on every surface.
    pub const fn hint(self) -> &'static str {
        match self {
            Self::UnknownDevice => "name a known device id, alias, or host:port (see `devices`)",
            Self::AmbiguousName => "use the full device id or a unique alias",
            Self::AmbiguousApp => "use the exact app id or a unique app name",
            Self::DeviceUnreachable => "check the device is powered and on this LAN",
            Self::DeviceSuspended => "the device stopped responding; retry or resume it",
            Self::TierRefused => "this operation needs a Permissive device",
            Self::ConfirmationRequired => "re-run with --confirm using an owner-minted token",
            Self::PolicyDenied => "the owner policy refuses this client/verb",
            Self::AppNotInstalled => "use an installed app id from `apps`",
            Self::StoreUnavailable => "the inventory store cannot be read; check the data dir",
            Self::LanBlockedByOsPolicy => "run through the resident daemon, not direct LAN",
            Self::DaemonUnavailable => "start the daemon or pass --direct for diagnosis",
            Self::DoctorWarn => "review the WARN findings and their remedies",
            Self::DoctorFail => "review the FAIL findings and their remedies",
        }
    }

    /// The CLI exit class carrying this failure. Per decision1213 the
    /// mapping is total but many-to-one: several semantic codes share one
    /// documented exit class; exits are never renumbered for the registry.
    /// Classes follow tested consumers: launch --app resolution refusals
    /// (APP_NOT_INSTALLED, AMBIGUOUS_APP) exit 2 alongside usage errors
    /// (cli_daemon_mutations refusal table); the daemon undo 404 path
    /// reports the same condition at exit 5 through its own contract.
    pub const fn cli_exit(self) -> i32 {
        match self {
            Self::UnknownDevice => 2,
            Self::AmbiguousName => 4,
            Self::AmbiguousApp => 2,
            Self::DeviceUnreachable => 5,
            Self::DeviceSuspended => 5,
            Self::TierRefused => 3,
            Self::ConfirmationRequired => 4,
            Self::PolicyDenied => 5,
            Self::AppNotInstalled => 2,
            Self::StoreUnavailable => 1,
            Self::LanBlockedByOsPolicy => 5,
            Self::DaemonUnavailable => 1,
            Self::DoctorWarn => 6,
            Self::DoctorFail => 7,
        }
    }

    /// Every registry code in declaration order (snapshot test).
    pub const fn all() -> [Self; 14] {
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
            Self::DoctorWarn,
            Self::DoctorFail,
        ]
    }

    /// All codes carried by one CLI exit class, in registry order. Empty
    /// for exits no code claims: callers must refuse unmapped classes
    /// rather than guess a code.
    pub fn codes_for_exit(exit: i32) -> Vec<Self> {
        Self::all()
            .into_iter()
            .filter(|c| c.cli_exit() == exit)
            .collect()
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
                "DOCTOR_WARN",
                "DOCTOR_FAIL",
            ]
        );
    }

    /// Total exit-class mapping per decision1213: every documented exit in
    /// {0, 1, 2, 3, 4, 5, 6, 7} accepts only registry codes, every exit in
    /// {2, 3, 4, 5, 6, 7} is claimed by at least one code, and the lookup
    /// rejects unmapped classes. Several codes share one documented class;
    /// exits are never renumbered for the registry.
    #[test]
    fn cli_exits_map_to_registry() {
        const DOCUMENTED_EXITS: [i32; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
        for code in ErrorCode::all() {
            assert!(
                DOCUMENTED_EXITS.contains(&code.cli_exit()),
                "{} maps to undocumented exit {}",
                code.as_str(),
                code.cli_exit()
            );
        }
        for exit in [2, 3, 4, 5, 6, 7] {
            assert!(
                !ErrorCode::codes_for_exit(exit).is_empty(),
                "CLI exit {exit} has no registry code"
            );
        }
        // Planted inputs: unknown exits are rejected by the lookup.
        assert!(ErrorCode::codes_for_exit(99).is_empty());
        assert!(ErrorCode::codes_for_exit(-1).is_empty());
    }
}
