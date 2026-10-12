//! Real fsqlite temp-file database; no mocks. Device identifiers below are
//! synthetic placeholders, never real serials/UDNs/MACs.

use zestroku_core::store::{
    AccessTierRow, ActionRow, AliasRow, AppRow, DeviceRow, DiffRow, MIGRATIONS, SqliteStore, Store,
    is_contiguous,
};

fn device(id: &str, tier: &str) -> DeviceRow {
    DeviceRow {
        id: id.into(),
        name: "Test Roku".into(),
        ip: "192.0.2.10".into(),
        model: "Express 4K+".into(),
        model_num: "3941X".into(),
        firmware: "15.0.0".into(),
        wifi_driver: "drv".into(),
        access_tier: tier.into(),
        last_seen: 1_700_000_000,
    }
}

fn action(result: &str) -> ActionRow {
    ActionRow {
        id: 0,
        at: 1_700_000_100,
        client: "cli".into(),
        surface: "keypress".into(),
        intent: "Home".into(),
        decision: "approved".into(),
        result: result.into(),
        before_state: "{\"app\":\"home\"}".into(),
    }
}

fn diff() -> DiffRow {
    DiffRow {
        id: 0,
        left_device: "dev-a".into(),
        right_device: "dev-b".into(),
        surface: "device-info".into(),
        field: "firmware".into(),
        left_value: "15.0.0".into(),
        right_value: "14.9.9".into(),
        ran_at: 1_700_000_200,
    }
}

#[test]
fn every_table_insert_query_update_delete_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zestroku.db");

    let st = SqliteStore::open(&path).unwrap();
    assert_eq!(st.schema_versions().unwrap(), vec![1, 2, 3, 4, 5, 6]);

    // devices: insert, query, update (upsert), delete
    st.upsert_device(&device("dev-a", "open")).unwrap();
    st.upsert_device(&device("dev-b", "permissive")).unwrap();
    assert_eq!(
        st.get_device("dev-a").unwrap().unwrap(),
        device("dev-a", "open")
    );
    st.upsert_device(&device("dev-a", "permissive")).unwrap();
    assert_eq!(
        st.get_device("dev-a").unwrap().unwrap().access_tier,
        "permissive"
    );
    assert_eq!(st.list_devices().unwrap().len(), 2);
    let alias = |device_id: &str| AliasRow {
        alias: "living-room".into(),
        device_id: device_id.into(),
    };
    st.set_alias(&alias("dev-a")).unwrap();
    st.set_alias(&alias("dev-b")).unwrap();
    assert_eq!(st.list_aliases().unwrap(), vec![alias("dev-b")]);

    // apps
    let app = |v: &str| AppRow {
        device_id: "dev-a".into(),
        app_id: "12".into(),
        name: "Netflix".into(),
        version: v.into(),
        fetched_at: 5,
    };
    st.upsert_app(&app("1.0")).unwrap();
    st.upsert_app(&app("2.0")).unwrap();
    assert_eq!(st.list_apps("dev-a").unwrap(), vec![app("2.0")]);

    // capabilities
    st.add_capability("dev-a", "keypress").unwrap();
    st.add_capability("dev-a", "launch").unwrap();
    st.add_capability("dev-a", "launch").unwrap();
    assert_eq!(
        st.list_capabilities("dev-a").unwrap(),
        vec!["keypress", "launch"]
    );
    st.remove_capability("dev-a", "keypress").unwrap();
    assert_eq!(st.list_capabilities("dev-a").unwrap(), vec!["launch"]);

    // access_tier
    let tier = |t: &str| AccessTierRow {
        device_id: "dev-a".into(),
        tier: t.into(),
        probe_at: 9,
        probe_map: "{\"launch\":\"open\",\"install\":\"403\"}".into(),
    };
    st.upsert_access_tier(&tier("limited")).unwrap();
    st.upsert_access_tier(&tier("permissive")).unwrap();
    assert_eq!(
        st.get_access_tier("dev-a").unwrap().unwrap(),
        tier("permissive")
    );

    // actions: insert, update, delete; ids distinct and assigned by store
    let a1 = st.insert_action(&action("pending")).unwrap();
    let a2 = st.insert_action(&action("pending")).unwrap();
    assert_ne!(a1, a2);
    st.update_action_result(a1, "ok").unwrap();
    assert_eq!(st.get_action(a1).unwrap().unwrap().result, "ok");
    assert_eq!(st.get_action(a2).unwrap().unwrap().result, "pending");

    // diff_results
    let d1 = st.insert_diff(&diff()).unwrap();
    let got = st.list_diffs().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0], DiffRow { id: d1, ..diff() });

    st.close().unwrap();

    // reopen: rows survive, migrations are not re-applied
    let st = SqliteStore::open(&path).unwrap();
    assert_eq!(st.schema_versions().unwrap(), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(st.list_devices().unwrap().len(), 2);
    assert_eq!(st.list_apps("dev-a").unwrap(), vec![app("2.0")]);
    assert_eq!(st.list_capabilities("dev-a").unwrap(), vec!["launch"]);
    assert!(st.get_access_tier("dev-a").unwrap().is_some());
    assert_eq!(st.list_actions().unwrap().len(), 2);
    assert_eq!(st.list_diffs().unwrap().len(), 1);
    assert_eq!(st.list_aliases().unwrap(), vec![alias("dev-b")]);
    st.delete_alias("living-room").unwrap();
    assert!(st.list_aliases().unwrap().is_empty());

    // deletes
    st.delete_app("dev-a", "12").unwrap();
    assert!(st.list_apps("dev-a").unwrap().is_empty());
    st.delete_access_tier("dev-a").unwrap();
    assert!(st.get_access_tier("dev-a").unwrap().is_none());
    st.delete_action(a1).unwrap();
    assert!(st.get_action(a1).unwrap().is_none());
    assert!(st.get_action(a2).unwrap().is_some());
    st.delete_diff(d1).unwrap();
    assert!(st.list_diffs().unwrap().is_empty());

    // delete_device cascades to dependents, leaves other devices alone
    st.upsert_app(&app("3.0")).unwrap();
    st.upsert_access_tier(&tier("open")).unwrap();
    st.set_alias(&alias("dev-a")).unwrap();
    st.delete_device("dev-a").unwrap();
    assert!(st.get_device("dev-a").unwrap().is_none());
    assert!(st.list_apps("dev-a").unwrap().is_empty());
    assert!(st.list_capabilities("dev-a").unwrap().is_empty());
    assert!(st.get_access_tier("dev-a").unwrap().is_none());
    assert!(st.get_device("dev-b").unwrap().is_some());
    assert!(st.list_aliases().unwrap().is_empty());
    st.close().unwrap();
}

#[test]
fn migrations_are_append_only_and_upgrade_in_place() {
    assert!(is_contiguous(MIGRATIONS));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("upgrade.db");

    // An older binary knew only migration 1: actions table must not exist yet.
    let st = SqliteStore::open_with(&path, &MIGRATIONS[..1]).unwrap();
    assert_eq!(st.schema_versions().unwrap(), vec![1]);
    st.upsert_device(&device("dev-a", "open")).unwrap();
    assert!(
        st.insert_action(&action("x")).is_err(),
        "migration 2 not applied yet"
    );
    st.close().unwrap();

    // The newer binary appends the remaining migrations and keeps existing data.
    let st = SqliteStore::open(&path).unwrap();
    assert_eq!(st.schema_versions().unwrap(), vec![1, 2, 3, 4, 5, 6]);
    assert!(st.get_device("dev-a").unwrap().is_some());
    st.insert_action(&action("x")).unwrap();
    st.close().unwrap();
}

#[test]
fn gapped_or_reordered_migration_list_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.db");
    // Deleting a middle step leaves a gap: must refuse, not silently skip.
    let gapped = [MIGRATIONS[0], MIGRATIONS[2]];
    assert!(!is_contiguous(&gapped));
    assert!(SqliteStore::open_with(&path, &gapped).is_err());
    let reordered = [MIGRATIONS[1], MIGRATIONS[0]];
    assert!(SqliteStore::open_with(&path, &reordered).is_err());
}

#[test]
fn identity_lookup_moves_aliases_to_the_new_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("identity.db");
    let st = SqliteStore::open(&path).unwrap();

    // Same physical device seen at its old endpoint first.
    st.upsert_device(&device("127.0.0.1:1111", "open")).unwrap();
    st.set_alias(&AliasRow {
        alias: "living-room".into(),
        device_id: "127.0.0.1:1111".into(),
    })
    .unwrap();
    st.record_device_identity("127.0.0.1:1111", "key-device-a")
        .unwrap();
    assert_eq!(
        st.find_device_id_by_identity("key-device-a").unwrap(),
        Some("127.0.0.1:1111".to_string())
    );

    // DHCP move: the same identity key reappears at a new endpoint.
    st.upsert_device(&device("127.0.0.1:2222", "open")).unwrap();
    st.rekey_aliases("127.0.0.1:1111", "127.0.0.1:2222")
        .unwrap();
    st.delete_device("127.0.0.1:1111").unwrap();
    st.delete_device_identity("127.0.0.1:1111").unwrap();
    st.record_device_identity("127.0.0.1:2222", "key-device-a")
        .unwrap();

    assert_eq!(st.list_devices().unwrap().len(), 1);
    assert_eq!(
        st.list_aliases().unwrap(),
        vec![AliasRow {
            alias: "living-room".into(),
            device_id: "127.0.0.1:2222".into(),
        }]
    );
    assert_eq!(
        st.find_device_id_by_identity("key-device-a").unwrap(),
        Some("127.0.0.1:2222".to_string())
    );
    st.close().unwrap();
}

#[test]
fn different_identities_are_never_merged() {
    // Planted negative: two same-model devices with different identity keys
    // must stay two rows. Merging on model (or dropping the key from the
    // lookup) turns this red.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no-merge.db");
    let st = SqliteStore::open(&path).unwrap();
    st.upsert_device(&device("127.0.0.1:1111", "open")).unwrap();
    st.upsert_device(&device("127.0.0.1:2222", "open")).unwrap();
    st.record_device_identity("127.0.0.1:1111", "key-device-a")
        .unwrap();
    st.record_device_identity("127.0.0.1:2222", "key-device-b")
        .unwrap();
    assert_eq!(st.list_devices().unwrap().len(), 2);
    assert_eq!(
        st.find_device_id_by_identity("key-device-a").unwrap(),
        Some("127.0.0.1:1111".to_string())
    );
    assert_eq!(
        st.find_device_id_by_identity("key-device-b").unwrap(),
        Some("127.0.0.1:2222".to_string())
    );
    assert_eq!(st.find_device_id_by_identity("key-unknown").unwrap(), None);
    st.close().unwrap();
}
