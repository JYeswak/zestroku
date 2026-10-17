//! Real temporary-database coverage for inserted-row identity across stores.
//! Trigger-created rows deterministically occupy the MAX(id) race window;
//! separate SqliteStore instances prove the returned keys address their own rows.

use fsqlite::AsyncConnection;
use zestroku_core::mutation::{MutationContext, MutationError, authorize_mutation_at};
use zestroku_core::policy::Policy;
use zestroku_core::store::{ActionRow, DiffRow, SqliteStore, Store};
use zestroku_proto::launch;
use zestroku_types::{AccessTier, Device};

fn action(intent: &str) -> ActionRow {
    ActionRow {
        id: 0,
        at: 1_700_000_100,
        client: "cli".into(),
        surface: "keypress".into(),
        intent: intent.into(),
        decision: "approved".into(),
        result: "pending".into(),
        before_state: "{\"app\":\"home\"}".into(),
    }
}

fn diff(field: &str) -> DiffRow {
    DiffRow {
        id: 0,
        left_device: "synthetic-a".into(),
        right_device: "synthetic-b".into(),
        surface: "device-info".into(),
        field: field.into(),
        left_value: "15.0.0".into(),
        right_value: "14.9.9".into(),
        ran_at: 1_700_000_200,
    }
}

#[test]
fn inserted_action_and_diff_ids_select_their_own_rows_across_connections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("identity.db");
    SqliteStore::open(&path).unwrap().close().unwrap();

    // The AFTER triggers create a distinct row before the store's method
    // returns. A global MAX(id) lookup therefore points at the trigger row,
    // while INSERT ... RETURNING id identifies the caller's row.
    let mut setup = AsyncConnection::open_sync(path.to_string_lossy().into_owned()).unwrap();
    setup
        .execute_batch_sync(
            "CREATE TRIGGER add_action_interloper AFTER INSERT ON actions \
             WHEN NEW.intent NOT LIKE 'fixture-shadow:%' \
             BEGIN INSERT INTO actions (at, client, surface, intent, decision, result, before_state) \
             VALUES (NEW.at, NEW.client, NEW.surface, 'fixture-shadow:' || NEW.intent, \
                     'fixture', 'pending', NEW.before_state); END; \
             CREATE TRIGGER add_diff_interloper AFTER INSERT ON diff_results \
             WHEN NEW.field NOT LIKE 'fixture-shadow:%' \
             BEGIN INSERT INTO diff_results (left_device, right_device, surface, field, \
                     left_value, right_value, ran_at) \
             VALUES (NEW.left_device, NEW.right_device, NEW.surface, \
                     'fixture-shadow:' || NEW.field, NEW.left_value, NEW.right_value, NEW.ran_at); END;",
        )
        .unwrap();
    setup.close_sync().unwrap();

    let first = SqliteStore::open(&path).unwrap();
    let second = SqliteStore::open(&path).unwrap();

    let first_action_id = first.insert_action(&action("Home")).unwrap();
    let second_action_id = second.insert_action(&action("Back")).unwrap();
    assert_ne!(first_action_id, second_action_id);
    assert_eq!(
        first.get_action(first_action_id).unwrap().unwrap().intent,
        "Home"
    );
    assert_eq!(
        second.get_action(second_action_id).unwrap().unwrap().intent,
        "Back"
    );

    // Complete through the other connection to model the consumer that uses
    // the returned action id after insertion.
    second
        .update_action_result(first_action_id, "completed")
        .unwrap();
    first
        .update_action_result(second_action_id, "completed")
        .unwrap();
    let actions = first.list_actions().unwrap();
    for intent in ["Home", "Back"] {
        assert_eq!(
            actions
                .iter()
                .find(|row| row.intent == intent)
                .unwrap()
                .result,
            "completed"
        );
    }
    for intent in ["fixture-shadow:Home", "fixture-shadow:Back"] {
        assert_eq!(
            actions
                .iter()
                .find(|row| row.intent == intent)
                .unwrap()
                .result,
            "pending"
        );
    }

    let first_diff_id = first.insert_diff(&diff("firmware")).unwrap();
    let second_diff_id = second.insert_diff(&diff("model")).unwrap();
    assert_ne!(first_diff_id, second_diff_id);
    let diffs = second.list_diffs().unwrap();
    assert_eq!(
        diffs
            .iter()
            .find(|row| row.id == first_diff_id)
            .unwrap()
            .field,
        "firmware"
    );
    assert_eq!(
        diffs
            .iter()
            .find(|row| row.id == second_diff_id)
            .unwrap()
            .field,
        "model"
    );
    first.close().unwrap();
    second.close().unwrap();
}

#[test]
fn busy_insert_retry_has_no_duplicate_action_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retry.db");
    let store = SqliteStore::open(&path).unwrap();
    let device = Device {
        host: "192.0.2.10".into(),
        port: 8060,
        model_name: "Test Roku".into(),
        model_number: "3941X".into(),
        software_version: "15.0.0".into(),
        is_tv: false,
        access_tier: AccessTier::Permissive,
    };
    let context = MutationContext {
        client: "unknown-client",
        surface: "launch",
        intent: "launch retry fixture",
        before_state: "{\"app\":\"home\"}",
    };

    // SQLite FAIL preserves earlier changes in the statement; the bounded
    // retry must not leave duplicated action rows after this reported error.
    let mut control = AsyncConnection::open_sync(path.to_string_lossy().into_owned()).unwrap();
    control
        .execute_batch_sync(
            "CREATE TRIGGER fail_retry_insert AFTER INSERT ON actions \
             WHEN NEW.intent = 'launch retry fixture' \
             BEGIN SELECT RAISE(FAIL, 'database is busy (fixture)'); END;",
        )
        .unwrap();
    control.close_sync().unwrap();

    let first = authorize_mutation_at(
        &store,
        &device,
        &launch(12),
        context,
        None,
        &Policy::default(),
        1_000,
    )
    .expect_err("the injected statement failure must exhaust the bounded retry");
    assert!(
        matches!(first, MutationError::Store(_)),
        "expected the stored error to propagate, got {first}"
    );
    assert!(
        first.to_string().contains("database is busy"),
        "unexpected injected error: {first}"
    );
    assert!(
        store.list_actions().unwrap().is_empty(),
        "aborted INSERT attempts must not leave committed action rows"
    );

    let mut control = AsyncConnection::open_sync(path.to_string_lossy().into_owned()).unwrap();
    control
        .execute_batch_sync("DROP TRIGGER fail_retry_insert;")
        .unwrap();
    control.close_sync().unwrap();

    let second = authorize_mutation_at(
        &store,
        &device,
        &launch(12),
        context,
        None,
        &Policy::default(),
        1_000,
    )
    .expect_err("the unresolved frontend must still be denied and audited");
    let action_id = match second {
        MutationError::PolicyDenied { action_id } => action_id,
        other => panic!("expected policy denial, got {other}"),
    };
    let actions = store.list_actions().unwrap();
    assert_eq!(actions.len(), 1);
    let row = store.get_action(action_id).unwrap().unwrap();
    assert_eq!(row.intent, "launch retry fixture");
    assert_eq!(row.decision, "denied");
    assert_eq!(row.result, "POLICY_DENIED");
    store.close().unwrap();
}
