use zestroku_core::{
    mutation::{
        MutationContext, MutationError, authorize_mutation, authorize_mutation_at, finish_mutation,
        issue_approval,
    },
    policy::Policy,
    store::{SqliteStore, Store},
};
use zestroku_proto::{Command, Method, launch, query};
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

    let error = authorize_mutation(
        &store,
        &device,
        &command,
        context(),
        None,
        &Policy::default(),
    )
    .unwrap_err();
    let action_id = match error {
        MutationError::ConfirmationRequired { action_id } => action_id,
        other => panic!("expected confirmation refusal, got {other}"),
    };
    let denied = store.get_action(action_id).unwrap().unwrap();
    assert_eq!(denied.decision, "denied");
    assert_eq!(denied.result, "CONFIRMATION_REQUIRED");
    assert_eq!(denied.before_state, "{\"app\":\"home\"}");

    let token = issue_approval(&store, &device, &command, 1_000).unwrap();
    let approved_id = authorize_mutation_at(
        &store,
        &device,
        &command,
        context(),
        Some(&token),
        &Policy::default(),
        1_001,
    )
    .unwrap();
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
        &Policy::default(),
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
        &Policy::default(),
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
            &Policy::default(),
            now,
        )
        .unwrap_err();
        assert!(matches!(err, MutationError::ConfirmationRequired { .. }));
    }

    let accepted = authorize_mutation_at(
        &store,
        &d,
        &launch(12),
        context(),
        Some(&token),
        &Policy::default(),
        11,
    )
    .unwrap();
    assert_eq!(
        store.get_action(accepted).unwrap().unwrap().decision,
        "approved"
    );
    let replay = authorize_mutation_at(
        &store,
        &d,
        &launch(12),
        context(),
        Some(&token),
        &Policy::default(),
        12,
    )
    .unwrap_err();
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

#[test]
fn unknown_client_and_verb_are_policy_denied_and_logged() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("zestroku.db")).unwrap();
    let device = device(AccessTier::Permissive);
    let policy = Policy::default();

    let stranger = MutationContext {
        client: "stranger",
        ..context()
    };
    let err = authorize_mutation_at(&store, &device, &launch(12), stranger, None, &policy, 1_000)
        .unwrap_err();
    let action_id = match err {
        MutationError::PolicyDenied { action_id } => action_id,
        other => panic!("expected policy denial, got {other}"),
    };
    let row = store.get_action(action_id).unwrap().unwrap();
    assert_eq!(row.decision, "denied");
    assert_eq!(row.result, "POLICY_DENIED");

    let reboot = Command {
        method: Method::Post,
        path: "/reboot".to_string(),
        tier: AccessTier::Permissive,
    };
    let err = authorize_mutation_at(&store, &device, &reboot, context(), None, &policy, 1_000)
        .unwrap_err();
    assert!(matches!(err, MutationError::PolicyDenied { .. }));
}

#[test]
fn burst_beyond_budget_is_refused_with_planted_negative() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("zestroku.db")).unwrap();
    let device = device(AccessTier::Permissive);
    let policy = Policy {
        launch_per_min: 2,
        ..Policy::default()
    };
    // Planted negative: removing the rate check turns the third approve green.
    let mut tokens = Vec::new();
    for _ in 0..3 {
        tokens.push(issue_approval(&store, &device, &launch(12), 500).unwrap());
    }
    for token in tokens.iter().take(2) {
        authorize_mutation_at(
            &store,
            &device,
            &launch(12),
            context(),
            Some(token),
            &policy,
            600,
        )
        .expect("within budget");
    }
    let err = authorize_mutation_at(
        &store,
        &device,
        &launch(12),
        context(),
        Some(&tokens[2]),
        &policy,
        600,
    )
    .unwrap_err();
    let action_id = match err {
        MutationError::PolicyDenied { action_id } => action_id,
        other => panic!("expected burst refusal, got {other}"),
    };
    assert_eq!(
        store.get_action(action_id).unwrap().unwrap().result,
        "POLICY_DENIED"
    );
    // Window slides: the same token class works a minute later.
    let later = issue_approval(&store, &device, &launch(12), 500).unwrap();
    authorize_mutation_at(
        &store,
        &device,
        &launch(12),
        context(),
        Some(&later),
        &policy,
        661,
    )
    .expect("budget refreshed after window");
}

#[test]
fn quiet_hours_require_fresh_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("zestroku.db")).unwrap();
    let device = device(AccessTier::Permissive);
    let policy = Policy {
        quiet: Some(zestroku_core::policy::QuietHours { start: 22, end: 7 }),
        ..Policy::default()
    };
    // Token minted 21:59, used 22:00:30: pre-window approval refused.
    let stale = issue_approval(&store, &device, &launch(12), 21 * 3600 + 59 * 60).unwrap();
    let err = authorize_mutation_at(
        &store,
        &device,
        &launch(12),
        context(),
        Some(&stale),
        &policy,
        22 * 3600 + 30,
    )
    .unwrap_err();
    assert!(matches!(err, MutationError::PolicyDenied { .. }));
    // Token minted 22:00:10, used 22:00:40: fresh confirmation approved.
    let fresh = issue_approval(&store, &device, &launch(12), 22 * 3600 + 10).unwrap();
    authorize_mutation_at(
        &store,
        &device,
        &launch(12),
        context(),
        Some(&fresh),
        &policy,
        22 * 3600 + 40,
    )
    .expect("fresh quiet-hours confirmation approves");
}

#[test]
fn concurrent_connections_share_one_budget() {
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zestroku.db");
    // Touch the file through one open so both threads share one DB.
    SqliteStore::open(&path).unwrap();
    let policy = Policy {
        launch_per_min: 1,
        ..Policy::default()
    };
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let path = path.clone();
            let policy = policy.clone();
            std::thread::spawn(move || {
                let store = SqliteStore::open(&path).unwrap();
                barrier.wait();
                authorize_mutation_at(
                    &store,
                    &device(AccessTier::Permissive),
                    &launch(12),
                    context(),
                    None,
                    &policy,
                    900,
                )
                .unwrap_err()
            })
        })
        .collect();
    let mut denied_rate = 0;
    let mut denied_confirm = 0;
    for worker in workers {
        match worker.join().expect("worker panicked") {
            MutationError::PolicyDenied { .. } => denied_rate += 1,
            MutationError::ConfirmationRequired { .. } => denied_confirm += 1,
            other => panic!("unexpected outcome: {other}"),
        }
    }
    // Exactly one thread won the single budget (then hit the token gate);
    // the loser was refused by the rate check. A split check-then-record
    // lets both threads observe remaining budget and record twice.
    assert_eq!(denied_rate, 1);
    assert_eq!(denied_confirm, 1);
}

#[test]
fn quiet_denial_preserves_the_unused_approval() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("zestroku.db")).unwrap();
    let device = device(AccessTier::Permissive);
    let quiet = Policy {
        quiet: Some(zestroku_core::policy::QuietHours { start: 22, end: 7 }),
        ..Policy::default()
    };
    // Minted 21:59, presented 22:00:30: refused, approval NOT consumed.
    // (Consume marks consumed_at but retains the row, and the peek does not
    // filter it, so a second quiet-denial would pass vacuously. Reuse with
    // quiet off instead: only a surviving token can approve.)
    let token = issue_approval(&store, &device, &launch(12), 21 * 3600 + 59 * 60).unwrap();
    let err = authorize_mutation_at(
        &store,
        &device,
        &launch(12),
        context(),
        Some(&token),
        &quiet,
        22 * 3600 + 30,
    )
    .unwrap_err();
    assert!(matches!(err, MutationError::PolicyDenied { .. }));
    // Same token, quiet off, still within TTL: approves, proving survival.
    // A consume-before-check implementation burns it on the denial above.
    let approved = authorize_mutation_at(
        &store,
        &device,
        &launch(12),
        context(),
        Some(&token),
        &Policy::default(),
        22 * 3600 + 40,
    )
    .expect("preserved approval authorizes with quiet off");
    assert_eq!(
        store.get_action(approved).unwrap().unwrap().decision,
        "approved"
    );
    // Replay now refused: single-use intact.
    let replay = authorize_mutation_at(
        &store,
        &device,
        &launch(12),
        context(),
        Some(&token),
        &Policy::default(),
        22 * 3600 + 50,
    )
    .unwrap_err();
    assert!(matches!(replay, MutationError::ConfirmationRequired { .. }));
}
