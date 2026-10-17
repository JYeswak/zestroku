use super::{
    http_free_port, mint_http_approval, post_json_to_daemon, seed_http_device, start_http_daemon,
};

#[test]
fn raw_http_cannot_spend_a_cli_bound_grant() {
    use zestroku_core::store::{SqliteStore, Store};
    use zestroku_proto::launch;
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("temporary client-bound database");
    let db = directory.path().join("daemon.db");
    let client_db = directory.path().join("client.db");
    let target = seed_http_device(&db, sim.port());
    let api_port = http_free_port();
    let api_host = format!("127.0.0.1:{api_port}");
    let daemon_url = format!("http://{api_host}");
    let _daemon = start_http_daemon(&db, api_port);
    let token = mint_http_approval(
        &db,
        sim.port(),
        &launch(1001),
        zestroku_core::mutation::ClientPrincipal::Cli,
    );
    let path = format!("/v1/devices/{target}/launch");

    // The raw HTTP helper spoofs X-Zestroku-Client: cli, but the omitted body
    // label defaults to loopback-http and cannot spend this CLI-bound grant.
    let (status, headers, body) = post_json_to_daemon(
        api_port,
        &path,
        &api_host,
        &serde_json::json!({"app_id": 1001, "confirm": &token}),
    );
    assert!(!headers.contains(&token));
    assert!(!body.contains(&token));
    assert_eq!(status, 403, "{body}");
    assert!(
        sim.received_requests()
            .iter()
            .all(|(method, _)| method != "POST"),
        "misattributed HTTP must produce no device mutation"
    );
    let denied = SqliteStore::open(&db)
        .expect("open audit after misattribution")
        .list_actions()
        .expect("read denial audit");
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].client, "unknown");
    assert_eq!(denied[0].decision, "denied");

    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &client_db)
        .env("ZESTROKU_DAEMON_URL", &daemon_url)
        .env_remove("ZESTROKU_DIRECT")
        .env_remove("ZESTROKU_TIER")
        .env_remove("ZESTROKU_TRANSPORT")
        .args(["launch", &target, "1001", "--confirm", &token])
        .output()
        .expect("run real CLI against resident daemon");
    assert!(!String::from_utf8_lossy(&cli.stdout).contains(&token));
    assert!(!String::from_utf8_lossy(&cli.stderr).contains(&token));
    assert_eq!(cli.status.code(), Some(0), "{cli:?}");
    let posts: Vec<_> = sim
        .received_requests()
        .into_iter()
        .filter_map(|(method, path)| (method == "POST").then_some(path))
        .collect();
    assert_eq!(posts, ["/launch/1001"]);
    let actions = SqliteStore::open(&db)
        .expect("open audit after CLI retry")
        .list_actions()
        .expect("read ordered actions");
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[1].client, "cli");
    assert_eq!(actions[1].decision, "approved");
    assert_eq!(actions[1].result, "completed");
    assert_eq!(sim.launch_count(), 1);
}

#[test]
fn http_bound_grant_refuses_a_forged_body_client_without_consumption() {
    use zestroku_core::mutation::ClientPrincipal;
    use zestroku_core::store::{SqliteStore, Store};
    use zestroku_proto::launch;
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let directory = tempfile::tempdir().expect("temporary HTTP principal database");
    let db = directory.path().join("daemon.db");
    let target = seed_http_device(&db, sim.port());
    let api_port = http_free_port();
    let api_host = format!("127.0.0.1:{api_port}");
    let _daemon = start_http_daemon(&db, api_port);
    let token = mint_http_approval(
        &db,
        sim.port(),
        &launch(1001),
        ClientPrincipal::LoopbackHttp,
    );
    let path = format!("/v1/devices/{target}/launch");

    let (status, headers, body) = post_json_to_daemon(
        api_port,
        &path,
        &api_host,
        &serde_json::json!({"app_id": 1001, "confirm": &token, "client": "cli"}),
    );
    assert!(!headers.contains(&token));
    assert!(!body.contains(&token));
    assert_eq!(status, 403, "{body}");
    assert!(
        sim.received_requests()
            .iter()
            .all(|(method, _)| method != "POST"),
        "a forged body principal must not mutate the device"
    );
    let denied = SqliteStore::open(&db)
        .expect("open body-spoof audit")
        .list_actions()
        .expect("read body-spoof denial");
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].client, "unknown");
    assert_eq!(denied[0].decision, "denied");

    // The same grant must remain usable by its owner-bound HTTP principal.
    let (status, headers, body) = post_json_to_daemon(
        api_port,
        &path,
        &api_host,
        &serde_json::json!({"app_id": 1001, "confirm": &token, "client": "loopback-http"}),
    );
    assert!(!headers.contains(&token));
    assert!(!body.contains(&token));
    assert_eq!(status, 200, "{body}");
    let posts: Vec<_> = sim
        .received_requests()
        .into_iter()
        .filter_map(|(method, path)| (method == "POST").then_some(path))
        .collect();
    assert_eq!(posts, ["/launch/1001"]);
    let actions = SqliteStore::open(&db)
        .expect("open reused HTTP grant audit")
        .list_actions()
        .expect("read completed HTTP action");
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[1].client, "loopback-http");
    assert_eq!(actions[1].decision, "approved");
    assert_eq!(actions[1].result, "completed");
    assert_eq!(sim.launch_count(), 1);
}
