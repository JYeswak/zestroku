//! Per-operation mutation authorization and durable action logging.
//!
//! The core checks the access tier before confirmation. Callers must capture
//! the active-app state before invoking this gate and pass it in the audit
//! context. Confirmed operations receive a pending action id for dispatch; the
//! caller records the outcome with [`finish_mutation`].

use std::time::{SystemTime, UNIX_EPOCH};

use zestroku_proto::{Command, Method};
use zestroku_types::{Device, Error};

use crate::store::{ActionRow, Store, StoreError};

pub const CONFIRMATION_REQUIRED: &str = "CONFIRMATION_REQUIRED";
pub const TIER_REFUSED: &str = "TIER_REFUSED";
pub const RESULT_PENDING: &str = "pending";

#[derive(Debug, thiserror::Error)]
pub enum MutationError {
    #[error(transparent)]
    TierRefused(#[from] Error),
    #[error("CONFIRMATION_REQUIRED (action {action_id})")]
    ConfirmationRequired { action_id: i64 },
    #[error("mutation gate only accepts POST commands")]
    NotMutation,
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

/// True for ECP commands that can mutate device state.
pub fn is_mutation(command: &Command) -> bool {
    command.method == Method::Post
}

/// Applies the tier gate before the per-operation confirmation gate and logs
/// each denial or approval through the durable store.
///
/// `confirmed` must only be true after explicit user confirmation at the CLI
/// or MCP boundary. This function does not perform network I/O.
pub fn authorize_mutation<S: Store + ?Sized>(
    store: &S,
    device: &Device,
    command: &Command,
    context: MutationContext<'_>,
    confirmed: bool,
) -> Result<i64, MutationError> {
    if !is_mutation(command) {
        return Err(MutationError::NotMutation);
    }

    if let Err(error) = crate::check_tier(command, device) {
        insert_action(store, &context, "denied", TIER_REFUSED)?;
        return Err(MutationError::TierRefused(error));
    }

    if !confirmed {
        let action_id = insert_action(store, &context, "denied", CONFIRMATION_REQUIRED)?;
        return Err(MutationError::ConfirmationRequired { action_id });
    }

    insert_action(store, &context, "approved", RESULT_PENDING)
}

/// Records the dispatch outcome, including an after-state snapshot when
/// available, in the existing action row's `result` field.
pub fn finish_mutation<S: Store + ?Sized>(
    store: &S,
    action_id: i64,
    result: &str,
) -> Result<(), MutationError> {
    store.update_action_result(action_id, result)?;
    Ok(())
}

fn insert_action<S: Store + ?Sized>(
    store: &S,
    context: &MutationContext<'_>,
    decision: &str,
    result: &str,
) -> Result<i64, MutationError> {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64;
    Ok(store.insert_action(&ActionRow {
        id: 0,
        at,
        client: context.client.to_owned(),
        surface: context.surface.to_owned(),
        intent: context.intent.to_owned(),
        decision: decision.to_owned(),
        result: result.to_owned(),
        before_state: context.before_state.to_owned(),
    })?)
}
