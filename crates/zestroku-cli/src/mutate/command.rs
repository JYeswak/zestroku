//! Pure mutation argument resolution; failures retain their semantic code and candidates.

use std::fmt;

use zestroku_core::mutation::MutationError;
use zestroku_proto::{Command, InstalledApp};
use zestroku_types::{AccessTier, ErrorCode};

#[derive(Debug)]
pub(super) enum AppResolutionError<'a> {
    NotInstalled,
    Ambiguous(Vec<&'a str>),
}

impl<'a> AppResolutionError<'a> {
    pub(super) const fn code(&self) -> ErrorCode {
        match self {
            Self::NotInstalled => ErrorCode::AppNotInstalled,
            Self::Ambiguous(_) => ErrorCode::AmbiguousApp,
        }
    }

    pub(super) fn suggestions(&self) -> &[&'a str] {
        match self {
            Self::NotInstalled => &[],
            Self::Ambiguous(candidates) => candidates,
        }
    }
}

impl fmt::Display for AppResolutionError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "no installed app matches the requested name"),
            Self::Ambiguous(candidates) => {
                write!(
                    f,
                    "requested name matches {} installed apps",
                    candidates.len()
                )
            }
        }
    }
}

pub(super) fn parse_app_id(target: &str) -> Result<u32, String> {
    target
        .parse()
        .map_err(|_| format!("app id must be a number, got {target:?}"))
}

pub(super) fn build_command(kind: &str, target: &str) -> Result<Command, String> {
    match kind {
        "launch" => parse_app_id(target).map(zestroku_proto::launch),
        "key" => Ok(zestroku_proto::keypress_raw(target)),
        "install" if target == "sideload" => Ok(Command {
            method: zestroku_proto::Method::Post,
            path: "/plugin_install".to_string(),
            tier: AccessTier::Permissive,
        }),
        "install" => Err("install approval target must be sideload".to_string()),
        other => Err(format!("unknown mutation kind {other}")),
    }
}

/// Exact case-insensitive names win; a prefix must identify exactly one app.
/// Neither a miss nor an ambiguous prefix may select a target for a POST.
pub(super) fn resolve_app_id<'a>(
    apps: &'a [InstalledApp],
    name: &str,
) -> Result<&'a str, AppResolutionError<'a>> {
    let want = name.to_lowercase();
    if let Some(app) = apps.iter().find(|app| app.name.to_lowercase() == want) {
        return Ok(app.id.as_str());
    }
    let mut matches = apps
        .iter()
        .filter(|app| app.name.to_lowercase().starts_with(&want));
    let Some(first) = matches.next() else {
        return Err(AppResolutionError::NotInstalled);
    };
    let Some(second) = matches.next() else {
        return Ok(first.id.as_str());
    };
    let candidates = std::iter::once(first.name.as_str())
        .chain(std::iter::once(second.name.as_str()))
        .chain(matches.map(|app| app.name.as_str()))
        .collect();
    Err(AppResolutionError::Ambiguous(candidates))
}

pub(crate) fn mutation_error_code(error: &MutationError) -> ErrorCode {
    match error {
        MutationError::TierRefused(error) => error.code(),
        MutationError::ConfirmationRequired { .. } => ErrorCode::ConfirmationRequired,
        MutationError::PolicyDenied { .. } => ErrorCode::PolicyDenied,
        MutationError::NotMutation => ErrorCode::ProtocolError,
        MutationError::TokenGeneration(_) => ErrorCode::RuntimeUnavailable,
        MutationError::Store(_) => ErrorCode::StoreUnavailable,
    }
}
