use zestroku_core::{
    mutation::{
        MutationContext, MutationError, authorize_mutation, authorize_mutation_at, finish_mutation,
        issue_approval,
    },
    store::{SqliteStore, Store},
};
use zestroku_proto::{launch, query};
use zestroku_types::{AccessTier, Device};

fn device(access_tier: AccessTier) -> Device {
    Device {
        host: "192.0.2.10".into(),
        port: 8060,
        model_name: "Test Roku".into(),
        model_number: "3941X".into(),
        software_version: "15.0.0".into(),
        is_tv: false,
        access_tier,
    }
}

fn context() -> MutationContext<'static> {
    MutationContext {
        client: "cli",
        surface: "launch",
        intent: "launch app 12",
        before_state: "{\"app\":\"home\"}",
    }
}

#[test]
fn confirmation_denial_is_logged_and_approved_result_is_updated() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("zestroku.db")).unwrap();
    let device = device(AccessTier::Permissive);
    let command = launch(12);

    let error = authorize_mutation(&store, &device, &command, context(), None).unwrap_err();
    let action_id = match error {
        MutationError::ConfirmationRequired { action_id } => action_id,
        other => panic!("expected confirmation refusal, got {other}"),
    };
    let denied = store.get_action(action_id).unwrap().unwrap();
    assert_eq!(denied.decision, "denied");
    assert_eq!(denied.result, "CONFIRMATION_REQUIRED");
    assert_eq!(denied.before_state, "{\"app\":\"home\"}");

    let token = issue_approval(&store, &device, &command, 1_000).unwrap();
    let approved_id =
        authorize_mutation_at(&store, &device, &command, context(), Some(&token), 1_001).unwrap();
    let approved = store.get_action(approved_id).unwrap().unwrap();
    assert_eq!(approved.decision, "approved");
    assert_eq!(approved.result, "pending");

    finish_mutation(&store, approved_id, "success; after_state={\"app_id\":12}").unwrap();
    assert_eq!(
        store.get_action(approved_id).unwrap().unwrap().result,
        "success; after_state={\"app_id\":12}"
    );
}

#[test]
fn tier_gate_precedes_confirmation_and_denial_is_logged() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("zestroku.db")).unwrap();
    let error = authorize_mutation(
        &store,
        &device(AccessTier::Open),
        &launch(12),
        context(),
        None,
    )
    .unwrap_err();
    assert!(matches!(error, MutationError::TierRefused(_)));

    let actions = store.list_actions().unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].decision, "denied");
    assert_eq!(actions[0].result, "TIER_REFUSED");
}

#[test]
fn read_commands_cannot_enter_mutation_gate() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("zestroku.db")).unwrap();
    let error = authorize_mutation(
        &store,
        &device(AccessTier::Open),
        &query("active-app"),
        context(),
        None,
    )
    .unwrap_err();
    assert!(matches!(error, MutationError::NotMutation));
    assert!(store.list_actions().unwrap().is_empty());
}

#[test]
fn approval_refuses_wrong_action_device_expiry_and_replay_with_audit_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("zestroku.db")).unwrap();
    let d = device(AccessTier::Permissive);
    let token = issue_approval(&store, &d, &launch(12), 10).unwrap();
    let mut other_device = device(AccessTier::Permissive);
    other_device.host = "192.0.2.11".into();

    for (candidate_device, command, now) in [
        (d.clone(), launch(13), 11),
        (other_device, launch(12), 11),
        (d.clone(), launch(12), 310),
    ] {
        let err = authorize_mutation_at(
            &store,
            &candidate_device,
            &command,
            context(),
            Some(&token),
            now,
        )
        .unwrap_err();
        assert!(matches!(err, MutationError::ConfirmationRequired { .. }));
    }

    let accepted =
        authorize_mutation_at(&store, &d, &launch(12), context(), Some(&token), 11).unwrap();
    assert_eq!(
        store.get_action(accepted).unwrap().unwrap().decision,
        "approved"
    );
    let replay =
        authorize_mutation_at(&store, &d, &launch(12), context(), Some(&token), 12).unwrap_err();
    assert!(matches!(replay, MutationError::ConfirmationRequired { .. }));

    let actions = store.list_actions().unwrap();
    assert_eq!(
        actions
            .iter()
            .filter(|row| row.decision == "denied")
            .count(),
        4
    );
    assert!(
        actions
            .iter()
            .filter(|row| row.decision == "denied")
            .all(|row| row.result == zestroku_core::mutation::CONFIRMATION_INVALID)
    );
}
