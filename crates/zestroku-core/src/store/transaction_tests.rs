use super::SqliteStore;
use fsqlite::FrankenError;

#[test]
fn commit_failure_rolls_back_before_same_connection_reuse() {
    let directory = tempfile::tempdir().expect("temporary Store database");
    let store = SqliteStore::open(&directory.path().join("commit-failure.db")).expect("open Store");
    store
        .conn
        .execute_batch_sync(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE commit_parent (id INTEGER PRIMARY KEY);
             CREATE TABLE commit_child (
                 parent_id INTEGER NOT NULL,
                 FOREIGN KEY(parent_id) REFERENCES commit_parent(id)
                     DEFERRABLE INITIALLY DEFERRED
             );",
        )
        .expect("create deferred-foreign-key fixture on Store connection");

    let body_completed = std::cell::Cell::new(false);
    let commit_error = store
        .in_transaction(|conn| {
            conn.execute_batch_sync("INSERT INTO commit_child(parent_id) VALUES (41)")?;
            body_completed.set(true);
            Ok(())
        })
        .expect_err("deferred foreign key must fail COMMIT, not the body");
    assert!(
        body_completed.get(),
        "deferred-FK error must happen after the transaction body completes"
    );
    assert!(
        commit_error
            .to_string()
            .to_ascii_lowercase()
            .contains("foreign key"),
        "expected a commit-time foreign-key error, got: {commit_error}"
    );
    assert!(
        !store.conn.in_transaction(),
        "failed COMMIT must be followed by cleanup on the same connection"
    );
    assert!(
        store
            .conn
            .query_sync("SELECT parent_id FROM commit_child")
            .expect("read child rows after failed COMMIT")
            .is_empty(),
        "failed transaction left a partial row"
    );

    store
        .in_transaction(|conn| {
            conn.execute_batch_sync(
                "INSERT INTO commit_parent(id) VALUES (41);
                 INSERT INTO commit_child(parent_id) VALUES (41);",
            )?;
            Ok(())
        })
        .expect("a fresh transaction must succeed on the same Store connection");
    assert_eq!(
        store
            .conn
            .query_sync("SELECT parent_id FROM commit_child")
            .expect("read committed child row")
            .len(),
        1
    );
    store.close().expect("close Store");
}

#[test]
fn body_error_rolls_back_and_preserves_primary_error() {
    let directory = tempfile::tempdir().expect("temporary Store database");
    let store = SqliteStore::open(&directory.path().join("body-failure.db")).expect("open Store");
    store
        .conn
        .execute_batch_sync("CREATE TABLE body_failure_rows (id INTEGER PRIMARY KEY);")
        .expect("create body-error fixture");

    let body_error = store
        .in_transaction(|conn| {
            conn.execute_batch_sync("INSERT INTO body_failure_rows(id) VALUES (1)")?;
            Err::<(), _>(FrankenError::Internal("injected body failure".into()))
        })
        .expect_err("body failure must propagate");
    assert!(body_error.to_string().contains("injected body failure"));
    assert!(
        !store.conn.in_transaction(),
        "body failure must leave the Store connection idle"
    );
    assert!(
        store
            .conn
            .query_sync("SELECT id FROM body_failure_rows")
            .expect("read after body rollback")
            .is_empty(),
        "body failure left a partial row"
    );
    store.close().expect("close Store");
}
#[test]
fn rollback_cleanup_error_keeps_the_primary_error() {
    let error = super::transaction_error(
        FrankenError::Internal("primary transaction error".into()),
        Err(FrankenError::Internal("rollback transport error".into())),
    );
    let message = error.to_string();
    assert!(
        message.contains("primary transaction error"),
        "cleanup failure replaced the primary error: {message}"
    );
    assert!(
        message.contains("transaction rollback cleanup failed"),
        "cleanup failure is not identified: {message}"
    );
    assert!(
        message.contains("rollback transport error"),
        "cleanup error was not retained: {message}"
    );
}
