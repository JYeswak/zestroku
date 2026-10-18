#![forbid(unsafe_code)]

use super::{
    Daemon, Session, free_port, initialize_session, is_error, mint_approval, result_json,
    seed_mutation_device,
};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::{Duration, Instant};
use zestroku_core::mutation::ClientPrincipal;
use zestroku_core::store::SqliteStore;
#[test]
fn real_frontends_select_only_their_own_allowlist_and_launch_budget() {
    use zestroku_core::policy::{LAUNCH_WINDOW_SECS, Policy};
    use zestroku_core::store::Store;
    use zestroku_proto::launch;
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let target = format!("127.0.0.1:{}", sim.port());
    let dir = tempfile::tempdir().expect("isolated frontend policy store");
    let db = dir.path().join("daemon.db");
    let client_dir = dir.path().join("empty-client");
    std::fs::create_dir(&client_dir).expect("isolated client without owner policy");
    let client_db = client_dir.join("client.db");
    seed_mutation_device(&db, &target, "SIM-1234X");
    let clients = [
        ClientPrincipal::Cli,
        ClientPrincipal::McpStdio,
        ClientPrincipal::LoopbackHttp,
    ];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64;
    let grants: [[String; 4]; 3] = clients.map(|client| {
        std::array::from_fn(|_| mint_approval(&db, &target, &launch(1001), now, client))
    });
    let policy_path = dir.path().join("policy.toml");
    let write_policy = |limit: u32, blocked: Option<&str>| {
        let mut text = format!("launch_per_min = {limit}\n[allow]\n");
        for client in clients {
            let verb = if blocked == Some(client.as_str()) {
                "keypress"
            } else {
                "launch"
            };
            text.push_str(&format!("{} = [\"{verb}\"]\n", client.as_str()));
        }
        Policy::parse(&text).expect("supported public owner-policy fixture");
        std::fs::write(&policy_path, text).expect("write daemon-owned policy");
    };
    write_policy(1, None);
    let port = free_port();
    let daemon_url = format!("http://127.0.0.1:{port}");
    let mut daemon = Daemon::start(&db, port);
    let mut session = Session::start(&client_db, &daemon_url);
    initialize_session(&mut session);
    let mut request_id = 2;
    let mut invoke = |client: ClientPrincipal, token: &str, permitted: bool| {
        let before = sim.launch_count();
        match client {
            ClientPrincipal::Cli => {
                let output = Command::new(env!("CARGO_BIN_EXE_zestroku"))
                    .env("ZESTROKU_DB", &client_db)
                    .env("ZESTROKU_DAEMON_URL", &daemon_url)
                    .env_remove("ZESTROKU_DIRECT")
                    .env_remove("ZESTROKU_TIER")
                    .env_remove("ZESTROKU_TRANSPORT")
                    .args(["launch", target.as_str(), "1001", "--confirm", token])
                    .output()
                    .expect("run actual CLI policy consumer");
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(!stdout.contains(token), "CLI exposed its approval");
                assert!(!stderr.contains(token), "CLI exposed its approval");
                assert_eq!(
                    output.status.code(),
                    Some(if permitted { 0 } else { 5 }),
                    "{output:?}"
                );
                if !permitted {
                    assert!(stdout.is_empty(), "a policy denial reported success");
                    assert!(stderr.contains("[POLICY_DENIED]"), "{stderr}");
                }
            }
            ClientPrincipal::McpStdio => {
                request_id += 1;
                let response = session.call(
                    request_id,
                    "launch",
                    json!({"host": target, "target": "1001", "confirm": token}),
                );
                assert!(
                    !response.to_string().contains(token),
                    "MCP exposed its approval"
                );
                assert_eq!(is_error(&response), !permitted, "{response}");
                if !permitted {
                    assert_eq!(result_json(&response)["code"], "POLICY_DENIED");
                }
            }
            ClientPrincipal::LoopbackHttp => {
                let body = json!({"app_id": 1001, "confirm": token}).to_string();
                let mut stream =
                    TcpStream::connect(("127.0.0.1", port)).expect("connect HTTP consumer");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("HTTP response bound");
                write!(
                    stream,
                    "POST /v1/devices/{target}/launch HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ).expect("send actual raw HTTP policy request");
                let mut raw = String::new();
                stream
                    .read_to_string(&mut raw)
                    .expect("read raw HTTP outcome");
                assert!(!raw.contains(token), "HTTP exposed its approval");
                let (headers, body) = raw.split_once("\r\n\r\n").expect("HTTP response separator");
                let status = headers.split_whitespace().nth(1).expect("HTTP status");
                assert_eq!(status, if permitted { "200" } else { "403" }, "{body}");
                if !permitted {
                    let error: Value =
                        serde_json::from_str(body).expect("structured HTTP policy error");
                    assert_eq!(error["code"], "POLICY_DENIED");
                }
            }
        }
        assert_eq!(
            sim.launch_count(),
            before + if permitted { 1 } else { 0 },
            "each allowed request dispatches once; a denied client dispatches nothing"
        );
    };

    let budget_started = Instant::now();
    for (index, client) in clients.into_iter().enumerate() {
        invoke(client, &grants[index][0], true);
        invoke(client, &grants[index][1], false);
    }
    assert!(
        budget_started.elapsed() < Duration::from_secs(LAUNCH_WINDOW_SECS as u64),
        "the budget control must complete before its measured hits can expire"
    );
    write_policy(20, None);
    for (index, client) in clients.into_iter().enumerate() {
        invoke(client, &grants[index][1], true);
    }
    for (index, client) in clients.into_iter().enumerate() {
        write_policy(20, Some(client.as_str()));
        invoke(client, &grants[index][2], false);
        let peer = (index + 1) % clients.len();
        invoke(clients[peer], &grants[peer][3], true);
        write_policy(20, None);
        invoke(client, &grants[index][2], true);
    }
    drop(session);
    daemon.stop();
    let rows = SqliteStore::open(&db)
        .expect("open completed policy audit")
        .list_actions()
        .expect("read actual frontend decisions");
    for token in grants.iter().flatten() {
        assert!(
            rows.iter().all(|row| !row.intent.contains(token)
                && !row.before_state.contains(token)
                && !row.result.contains(token)),
            "the policy audit exposed a fixture approval"
        );
    }
    assert_eq!(rows.len(), 18);
    for client in clients {
        let (mut total, mut approved, mut denied) = (0, 0, 0);
        for row in rows.iter().filter(|row| row.client == client.as_str()) {
            total += 1;
            approved += usize::from(row.decision == "approved");
            denied += usize::from(row.decision == "denied");
            if row.decision == "approved" {
                assert_eq!(row.result, "completed");
            } else if row.decision == "denied" {
                assert_eq!(row.result, "POLICY_DENIED");
            }
        }
        assert_eq!(total, 6, "each real frontend has its own audited decisions");
        assert_eq!(approved, 4);
        assert_eq!(denied, 2);
    }
}
