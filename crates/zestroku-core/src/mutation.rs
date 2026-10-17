//! Per-operation mutation authorization and durable action logging.
//!
//! Approval tokens are random, single-use, and bound to the canonical device
//! ID and exact ECP command path. Only token hashes are persisted.

use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use zestroku_proto::{Command, Method};
use zestroku_types::{Device, Error};

use crate::policy::Policy;
use crate::store::{ActionRow, ConfirmationTokenRow, Store, StoreError};

pub const CONFIRMATION_REQUIRED: &str = "CONFIRMATION_REQUIRED";
pub const CONFIRMATION_INVALID: &str = "CONFIRMATION_INVALID";
pub const TIER_REFUSED: &str = "TIER_REFUSED";
pub const POLICY_DENIED: &str = "POLICY_DENIED";
pub const RESULT_PENDING: &str = "pending";
pub const APPROVAL_TTL_SECS: i64 = 300;

#[derive(Debug, thiserror::Error)]
pub enum MutationError {
    #[error(transparent)]
    TierRefused(#[from] Error),
    #[error("CONFIRMATION_REQUIRED (action {action_id})")]
    ConfirmationRequired { action_id: i64 },
    #[error("POLICY_DENIED (action {action_id})")]
    PolicyDenied { action_id: i64 },
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
    policy: &Policy,
) -> Result<i64, MutationError> {
    let now = epoch_seconds();
    authorize_mutation_at(store, device, command, context, token, policy, now)
}

/// Deterministic-time variant used by expiry and single-use tests.
pub fn authorize_mutation_at<S: Store + ?Sized>(
    store: &S,
    device: &Device,
    command: &Command,
    context: MutationContext<'_>,
    token: Option<&str>,
    policy: &Policy,
    now: i64,
) -> Result<i64, MutationError> {
    if !is_mutation(command) {
        return Err(MutationError::NotMutation);
    }
    if let Err(error) = crate::check_tier(command, device) {
        insert_action(store, &context, "denied", TIER_REFUSED)?;
        return Err(MutationError::TierRefused(error));
    }

    // Shared owner-policy choke (o0r): allowlist, then rate budget, both
    // before any token is consumed. Denials are audited POLICY_DENIED. The
    // rate check-and-record is one store transaction: separate CLI processes
    // must not both observe remaining budget (TOCTOU). Transient lock
    // contention is retried immediately and boundedly (no sleep: the holder
    // commits in microseconds); exhaustion or real failures propagate, never
    // deny and never swallow.
    let verb = Policy::verb_of(&command.path).unwrap_or("");
    if !policy.allows(context.client, verb) {
        let action_id = insert_action(store, &context, "denied", POLICY_DENIED)?;
        return Err(MutationError::PolicyDenied { action_id });
    }
    if let Some((budget, window)) = policy.budget(verb) {
        let since = now.saturating_sub(window);
        if !try_record_with_retry(store, context.client, verb, since, budget as i64, now)? {
            let action_id = insert_action(store, &context, "denied", POLICY_DENIED)?;
            return Err(MutationError::PolicyDenied { action_id });
        }
    }

    // Quiet hours gate BEFORE any token is consumed: the presented token
    // must have been minted inside an active window (fresh explicit
    // confirmation, UTC hours). A quiet-denied attempt preserves the
    // single-use approval for later use.
    if let Some(token) = token.filter(|t| !t.trim().is_empty()) {
        if let Some(quiet) = &policy.quiet {
            let digest = Sha256::digest(token.as_bytes());
            if let Some(expires_at) = store.peek_confirmation_token_expiry(&hex(&digest))? {
                let minted_at = expires_at.saturating_sub(APPROVAL_TTL_SECS);
                if quiet.active_at(hour_of_day_utc(now))
                    && !quiet.active_at(hour_of_day_utc(minted_at))
                {
                    let action_id = insert_action(store, &context, "denied", POLICY_DENIED)?;
                    return Err(MutationError::PolicyDenied { action_id });
                }
            }
        }
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

/// UTC hour of day for an epoch timestamp.
fn hour_of_day_utc(now: i64) -> u8 {
    (now.rem_euclid(86_400) / 3_600) as u8
}

/// Bounded immediate attempts for one store mutation.
const STORE_RETRY_BOUND: u32 = 64;

/// Run one atomic rate check-and-record, retrying transient lock contention
/// immediately (no sleep: the holder commits in microseconds) up to
/// STORE_RETRY_BOUND. Budget exhaustion returns false (deny + audit at the
/// call site); real failures propagate after the bound, never deny.
fn try_record_with_retry<S: Store + ?Sized>(
    store: &S,
    client: &str,
    verb: &str,
    since: i64,
    budget: i64,
    now: i64,
) -> Result<bool, MutationError> {
    retry_on_busy(|| {
        store
            .try_record_policy_hit(client, verb, since, budget, now)
            .map_err(MutationError::Store)
    })
}

/// Retry a fallible store mutation on transient lock contention between
/// separate writers (bounded, no sleep). A busy transaction rolls back, so
/// single-statement retries never double-apply; exhaustion and real failures
/// propagate, never deny and never swallow.
fn retry_on_busy<T>(mut op: impl FnMut() -> Result<T, MutationError>) -> Result<T, MutationError> {
    let mut attempts = 0;
    loop {
        match op() {
            Err(MutationError::Store(error))
                if is_lock_contention(&error) && attempts < STORE_RETRY_BOUND =>
            {
                attempts += 1;
            }
            result => return result,
        }
    }
}

/// True for transient lock contention between separate writers; anything
/// else is a real failure and must propagate, never deny.
fn is_lock_contention(error: &StoreError) -> bool {
    error.to_string().contains("database is busy")
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
    let row = ActionRow {
        id: 0,
        at: epoch_seconds(),
        client: context.client.to_owned(),
        surface: context.surface.to_owned(),
        intent: context.intent.to_owned(),
        decision: decision.to_owned(),
        result: result.to_owned(),
        before_state: context.before_state.to_owned(),
    };
    retry_on_busy(|| store.insert_action(&row).map_err(MutationError::Store))
}
