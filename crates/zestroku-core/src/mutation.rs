//! Per-operation mutation authorization and durable action logging.
//!
//! Approval tokens are random, single-use, and bound to the canonical device
//! ID and exact ECP command path. Only token hashes are persisted.

use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use zestroku_proto::{Command, Method};
use zestroku_types::{Device, Error};

use crate::store::{ActionRow, ConfirmationTokenRow, Store, StoreError};

pub const CONFIRMATION_REQUIRED: &str = "CONFIRMATION_REQUIRED";
pub const CONFIRMATION_INVALID: &str = "CONFIRMATION_INVALID";
pub const TIER_REFUSED: &str = "TIER_REFUSED";
pub const RESULT_PENDING: &str = "pending";
pub const APPROVAL_TTL_SECS: i64 = 300;

#[derive(Debug, thiserror::Error)]
pub enum MutationError {
    #[error(transparent)]
    TierRefused(#[from] Error),
    #[error("CONFIRMATION_REQUIRED (action {action_id})")]
    ConfirmationRequired { action_id: i64 },
    #[error("mutation gate only accepts POST commands")]
    NotMutation,
    #[error("could not generate approval token: {0}")]
    TokenGeneration(String),
    #[error("action log: {0}")]
    Store(#[from] StoreError),
}

/// Audit context supplied by a CLI or MCP boundary.
pub struct MutationContext<'a> {
    pub client: &'a str,
    pub surface: &'a str,
    pub intent: &'a str,
    /// Active-app snapshot captured before dispatch.
    pub before_state: &'a str,
}

pub fn is_mutation(command: &Command) -> bool {
    command.method == Method::Post
}

/// Mint a 256-bit token bound to one device and the exact command path.
/// `now` is injectable so expiry behavior is deterministic in tests.
pub fn issue_approval<S: Store + ?Sized>(
    store: &S,
    device: &Device,
    command: &Command,
    now: i64,
) -> Result<String, MutationError> {
    if !is_mutation(command) {
        return Err(MutationError::NotMutation);
    }
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| MutationError::TokenGeneration(e.to_string()))?;
    let token = hex(&bytes);
    let digest = Sha256::digest(token.as_bytes());
    let row = ConfirmationTokenRow {
        token_hash: hex(&digest),
        device_id: device_id(device),
        action: command.path.clone(),
        expires_at: now.saturating_add(APPROVAL_TTL_SECS),
    };
    store.insert_confirmation_token(&row)?;
    Ok(token)
}

/// Applies the tier gate before consuming a matching approval token.
pub fn authorize_mutation<S: Store + ?Sized>(
    store: &S,
    device: &Device,
    command: &Command,
    context: MutationContext<'_>,
    token: Option<&str>,
) -> Result<i64, MutationError> {
    let now = epoch_seconds();
    authorize_mutation_at(store, device, command, context, token, now)
}

/// Deterministic-time variant used by expiry and single-use tests.
pub fn authorize_mutation_at<S: Store + ?Sized>(
    store: &S,
    device: &Device,
    command: &Command,
    context: MutationContext<'_>,
    token: Option<&str>,
    now: i64,
) -> Result<i64, MutationError> {
    if !is_mutation(command) {
        return Err(MutationError::NotMutation);
    }
    if let Err(error) = crate::check_tier(command, device) {
        insert_action(store, &context, "denied", TIER_REFUSED)?;
        return Err(MutationError::TierRefused(error));
    }

    let accepted = token
        .filter(|t| !t.trim().is_empty())
        .map(|t| {
            let digest = Sha256::digest(t.as_bytes());
            store.consume_confirmation_token(&hex(&digest), &device_id(device), &command.path, now)
        })
        .transpose()?
        .unwrap_or(false);
    if !accepted {
        let reason = if token.is_some_and(|t| !t.trim().is_empty()) {
            CONFIRMATION_INVALID
        } else {
            CONFIRMATION_REQUIRED
        };
        let action_id = insert_action(store, &context, "denied", reason)?;
        return Err(MutationError::ConfirmationRequired { action_id });
    }
    insert_action(store, &context, "approved", RESULT_PENDING)
}

pub fn finish_mutation<S: Store + ?Sized>(
    store: &S,
    action_id: i64,
    result: &str,
) -> Result<(), MutationError> {
    store.update_action_result(action_id, result)?;
    Ok(())
}

fn device_id(device: &Device) -> String {
    format!("{}:{}", device.host, device.port)
}

fn epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let byte = *byte;
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn insert_action<S: Store + ?Sized>(
    store: &S,
    context: &MutationContext<'_>,
    decision: &str,
    result: &str,
) -> Result<i64, MutationError> {
    Ok(store.insert_action(&ActionRow {
        id: 0,
        at: epoch_seconds(),
        client: context.client.to_owned(),
        surface: context.surface.to_owned(),
        intent: context.intent.to_owned(),
        decision: decision.to_owned(),
        result: result.to_owned(),
        before_state: context.before_state.to_owned(),
    })?)
}
