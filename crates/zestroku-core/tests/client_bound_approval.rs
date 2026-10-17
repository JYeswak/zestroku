use fsqlite::{AsyncConnection, SqliteValue};
use sha2::{Digest, Sha256};
use zestroku_core::{
    mutation::{MutationContext, MutationError, authorize_mutation_at},
    policy::Policy,
    store::{MIGRATIONS, SqliteStore, Store},
};
use zestroku_proto::launch;
use zestroku_types::{AccessTier, Device};

fn legacy_token_hash(token: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(token.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[test]
fn unbound_legacy_grant_stays_unresolved_and_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zestroku.db");
    let old_store = SqliteStore::open_with(&path, &MIGRATIONS[..7]).unwrap();
    old_store.close().unwrap();
    let device = Device {
        host: "192.0.2.10".into(),
        port: 8060,
        model_name: "Test Roku".into(),
        model_number: "3941X".into(),
        software_version: "15.0.0".into(),
        is_tv: false,
        access_tier: AccessTier::Permissive,
    };
    let command = launch(12);
    let token = "legacy-client-unbound-token";
    let token_hash = legacy_token_hash(token);
    let old_conn = AsyncConnection::open_sync(path.to_string_lossy().into_owned()).unwrap();
    old_conn
        .execute_with_params_sync(
            "INSERT INTO confirmation_tokens (token_hash, device_id, action, expires_at, consumed_at) VALUES (?1, ?2, ?3, ?4, NULL)",
            &[
                SqliteValue::Text(token_hash.clone()),
                SqliteValue::Text(format!("{}:{}", device.host, device.port)),
                SqliteValue::Text(command.path.clone()),
                SqliteValue::Integer(1_300),
            ],
        )
        .unwrap();
    drop(old_conn);
    let store = SqliteStore::open(&path).unwrap();
    let mut policy = Policy::default();
    policy
        .allow
        .insert("mcp-stdio".into(), vec!["launch".into()]);

    let error = authorize_mutation_at(
        &store,
        &device,
        &command,
        MutationContext {
            client: "mcp-stdio",
            surface: "launch",
            intent: "launch app 12",
            before_state: "{}",
        },
        Some(token),
        &policy,
        1_001,
    )
    .expect_err("an unbound legacy token must not authorize a claimed client");
    let action_id = match error {
        MutationError::ConfirmationRequired { action_id } => action_id,
        other => panic!("expected an unbound-grant refusal, got {other}"),
    };
    let denial = store.get_action(action_id).unwrap().unwrap();
    assert_eq!(denial.client, "unknown");
    assert_eq!(denial.result, zestroku_core::mutation::CONFIRMATION_INVALID);

    assert_eq!(
        store.peek_confirmation_token_expiry(&token_hash).unwrap(),
        Some(1_300),
        "the denied attempt must leave the old token row present"
    );
}
