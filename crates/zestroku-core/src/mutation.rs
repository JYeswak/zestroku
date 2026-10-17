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

/// The closed set of frontends that can receive owner-issued mutation grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientPrincipal {
    Cli,
    McpStdio,
    LoopbackHttp,
}

impl ClientPrincipal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::McpStdio => "mcp-stdio",
            Self::LoopbackHttp => "loopback-http",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cli" => Some(Self::Cli),
            "mcp-stdio" => Some(Self::McpStdio),
            "loopback-http" => Some(Self::LoopbackHttp),
            _ => None,
        }
    }
}

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

/// Expected frontend label. It is trusted only when its client-bound grant exists.
#[derive(Clone, Copy)]
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

/// Mint a 256-bit token bound to one device, the exact command path, and one frontend.
/// Legacy rows keep a NULL client binding and are never assigned a default.
/// `now` is injectable so expiry behavior is deterministic in tests.
pub fn issue_approval<S: Store + ?Sized>(
    store: &S,
    device: &Device,
    command: &Command,
    client: ClientPrincipal,
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
        client: Some(client.as_str().to_owned()),
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
    let token = token.filter(|candidate| !candidate.trim().is_empty());
    let expected_client = ClientPrincipal::parse(context.client);
    let (token_hash, grant) = match (expected_client, token) {
        (Some(_), Some(token)) => {
            let digest = Sha256::digest(token.as_bytes());
            let token_hash = hex(&digest);
            let grant = store.peek_confirmation_grant(&token_hash)?;
            (Some(token_hash), grant)
        }
        _ => (None, None),
    };
    let stored_client = grant.as_ref().and_then(|grant| grant.client.as_deref());
    let grant_expiry = grant.as_ref().map(|grant| grant.expires_at);
    let trusted_client = match (expected_client, stored_client.as_deref(), grant_expiry) {
        (Some(expected), Some(stored), Some(_)) if stored == expected.as_str() => Some(expected),
        _ => None,
    };
    let audit_context = MutationContext {
        client: trusted_client.map_or("unknown", ClientPrincipal::as_str),
        ..context
    };

    // Tier refusal remains before any approval consumption. The frontend
    // identity is trusted only when the stored grant binds the same client.
    if let Err(error) = crate::check_tier(command, device) {
        insert_action(store, &audit_context, "denied", TIER_REFUSED)?;
        return Err(MutationError::TierRefused(error));
    }

    if expected_client.is_none() {
        let action_id = insert_action(store, &audit_context, "denied", POLICY_DENIED)?;
        return Err(MutationError::PolicyDenied { action_id });
    }
    if token.is_none() {
        let action_id = insert_action(store, &audit_context, "denied", CONFIRMATION_REQUIRED)?;
        return Err(MutationError::ConfirmationRequired { action_id });
    }
    let Some(client) = trusted_client else {
        let action_id = insert_action(store, &audit_context, "denied", CONFIRMATION_INVALID)?;
        return Err(MutationError::ConfirmationRequired { action_id });
    };
    let Some(token_hash) = token_hash.as_deref() else {
        let action_id = insert_action(store, &audit_context, "denied", CONFIRMATION_INVALID)?;
        return Err(MutationError::ConfirmationRequired { action_id });
    };

    // Shared owner-policy choke (o0r): allowlist, then rate budget, both
    // before any token is consumed. The client key comes from the matching
    // stored grant, not from the frontend label alone.
    let verb = Policy::verb_of(&command.path).unwrap_or("");
    if !policy.allows(client.as_str(), verb) {
        let action_id = insert_action(store, &audit_context, "denied", POLICY_DENIED)?;
        return Err(MutationError::PolicyDenied { action_id });
    }
    if let Some((budget, window)) = policy.budget(verb) {
        // Half-open window (now-window, now]: a hit exactly window-old is
        // expired. The store counts at>=since, so the bound is exclusive.
        let since = now.saturating_sub(window).saturating_add(1);
        if !try_record_with_retry(store, client.as_str(), verb, since, budget as i64, now)? {
            let action_id = insert_action(store, &audit_context, "denied", POLICY_DENIED)?;
            return Err(MutationError::PolicyDenied { action_id });
        }
    }

    // Quiet hours gate BEFORE any token is consumed. A quiet-denied attempt
    // preserves the single-use approval for later use.
    if let (Some(expires_at), Some(quiet)) = (grant_expiry, &policy.quiet) {
        let minted_at = expires_at.saturating_sub(APPROVAL_TTL_SECS);
        if quiet.active_at(hour_of_day_utc(now)) && !quiet.active_at(hour_of_day_utc(minted_at)) {
            let action_id = insert_action(store, &audit_context, "denied", POLICY_DENIED)?;
            return Err(MutationError::PolicyDenied { action_id });
        }
    }
    let accepted = store.consume_confirmation_token(
        token_hash,
        &device_id(device),
        &command.path,
        client.as_str(),
        now,
    )?;
    if !accepted {
        let action_id = insert_action(store, &audit_context, "denied", CONFIRMATION_INVALID)?;
        return Err(MutationError::ConfirmationRequired { action_id });
    }
    insert_action(store, &audit_context, "approved", RESULT_PENDING)
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
        client: context.client.to_owned(),
        surface: context.surface.to_owned(),
        intent: context.intent.to_owned(),
        decision: decision.to_owned(),
        result: result.to_owned(),
        before_state: context.before_state.to_owned(),
    };
    retry_on_busy(|| store.insert_action(&row).map_err(MutationError::Store))
}
