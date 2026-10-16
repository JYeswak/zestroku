//! `zestroku mcp` speaks MCP over stdio: spawns the real binary, initializes,
//! lists tools, round-trips `echo`, and reads `list_devices`, `info`,
//! `active_app`, and `apps` through the real daemon and simulator. It checks
//! unavailable-daemon behavior without fallback and proves an unconfirmed
//! `key` is refused with CONFIRMATION_REQUIRED and audited.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use zestroku_core::store::{DeviceRow, SqliteStore, Store};

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Session {
    fn start(db: &Path, daemon_url: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zestroku"))
            .arg("mcp")
            .env("ZESTROKU_DB", db)
            .env("ZESTROKU_DAEMON_URL", daemon_url)
            // Operator tier override, as the CLI honors: the gate under test
            // is the confirmation gate, not tier discovery.
            .env("ZESTROKU_TIER", "permissive")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn zestroku mcp");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout");
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{msg}").expect("write request");
        stdin.flush().expect("flush");
    }

    /// Next JSON-RPC message carrying `id`.
    fn response(&self, id: u64) -> Value {
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(30))
                .expect("server answered within 30s");
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                panic!("non-JSON on MCP stdout: {line}");
            };
            if msg["id"] == id {
                return msg;
            }
        }
    }

    fn call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }));
        self.response(id)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    listener.local_addr().expect("free port address").port()
}

struct Daemon(Child);

impl Daemon {
    fn start(db: &Path, port: u16) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_zestroku"))
            .env("ZESTROKU_DB", db)
            .args(["serve", "--bind", &format!("127.0.0.1:{port}")])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn zestroku serve");
        let daemon = Self(child);
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return daemon;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("serve did not listen on 127.0.0.1:{port} within 30s");
    }

    fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn seed_device(db: &Path, target: &str, model_number: &str) {
    let host = target.rsplit_once(':').map_or(target, |(host, _)| host);
    SqliteStore::open(db)
        .expect("open sim inventory")
        .upsert_device(&DeviceRow {
            id: target.to_owned(),
            name: "Sim Roku".to_owned(),
            ip: host.to_owned(),
            model: "Roku Express".to_owned(),
            model_num: model_number.to_owned(),
            firmware: "15.2.4".to_owned(),
            wifi_driver: "fixture".to_owned(),
            access_tier: "permissive".to_owned(),
            last_seen: 1,
        })
        .expect("seed sim device");
}

fn seed_mutation_device(db: &Path, target: &str, model_number: &str) {
    use zestroku_core::store::AccessTierRow;

    seed_device(db, target, model_number);
    let (host, port) = target
        .rsplit_once(':')
        .expect("sim target has host and port");
    let port = port.parse::<u16>().expect("sim target port");
    SqliteStore::open(db)
        .expect("open sim tier store")
        .upsert_access_tier(&AccessTierRow {
            device_id: zestroku_core::tier::device_id(host, port),
            tier: "permissive".to_owned(),
            probe_at: 1,
            probe_map: r#"{"query/apps":"open"}"#.to_owned(),
        })
        .expect("seed sim access tier");
}

fn mint_mcp_approval(
    db: &Path,
    target: &str,
    command: &zestroku_proto::Command,
    issued_at: i64,
) -> String {
    use zestroku_core::mutation::issue_approval;
    use zestroku_types::{AccessTier, Device};

    let (host, port) = target
        .rsplit_once(':')
        .expect("sim target has host and port");
    let port = port.parse::<u16>().expect("sim target port");
    let store = SqliteStore::open(db).expect("open sim approval store");
    issue_approval(
        &store,
        &Device {
            host: host.to_owned(),
            port,
            model_name: "Sim Express".to_owned(),
            model_number: "SIMX1".to_owned(),
            software_version: "15.2.4".to_owned(),
            is_tv: false,
            access_tier: AccessTier::Permissive,
        },
        command,
        issued_at,
    )
    .expect("mint one-use MCP approval")
}
/// Mint a real CLI approval through `script` so the terminal-only CLI gate
/// stays active in this process-level regression.
fn mint_cli_approval(db: &Path, target: &str, key: &str, transcript: &Path) -> String {
    let binary = env!("CARGO_BIN_EXE_zestroku");
    let mut command = Command::new("script");
    #[cfg(target_os = "macos")]
    command
        .args(["-q", "-e"])
        .arg(transcript)
        .arg(binary)
        .args(["approve", "key", target, key]);
    #[cfg(target_os = "linux")]
    command
        .args(["-q", "-e", "-c"])
        .arg(format!("{binary} approve key {target} {key}"))
        .arg(transcript);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    panic!("CLI approval PTY test requires macOS or Linux `script`");

    let mut child = command
        .env("ZESTROKU_DB", db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start CLI approval under PTY");
    let phrase = format!("approve key {target} {key}\n");
    child
        .stdin
        .take()
        .expect("PTY stdin")
        .write_all(phrase.as_bytes())
        .expect("write exact approval phrase");
    let output = child.wait_with_output().expect("wait for CLI approval");
    assert!(
        output.status.success(),
        "CLI approval failed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find(|word| word.len() == 64 && word.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            panic!(
                "CLI approval emitted no token; stdout={}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
}

fn initialize_session(session: &mut Session) {
    session.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "zestroku-mutation-test", "version": "0"}
        }
    }));
    let initialized = session.response(1);
    assert_eq!(
        initialized["result"]["serverInfo"]["name"], "zestroku",
        "{initialized}"
    );
    session.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
}
fn result_json(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("missing text result: {response}"));
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tool returned non-JSON text ({error}): {text}"))
}

fn listed_tool<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("missing tool {name}: {list}"))
}

fn is_error(resp: &Value) -> bool {
    resp["error"].is_object() || resp["result"]["isError"] == true
}

fn tool_error_text(response: &Value) -> &str {
    assert_eq!(response["result"]["isError"], true, "{response}");
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool error omitted readable content: {response}"))
}

#[test]
fn mcp_stdio_routes_reads_through_daemon_and_preserves_gate() {
    let sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let target = format!("127.0.0.1:{}", sim.port());
    let mut unreachable_sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let unreachable_target = format!("127.0.0.1:{}", unreachable_sim.port());
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_mutation_device(&db, &target, "SIM-1234X");
    seed_device(&db, &unreachable_target, "SIM-DEAD");
    unreachable_sim.suspend();

    let daemon_port = free_port();
    let mut daemon = Daemon::start(&db, daemon_port);
    let daemon_url = format!("http://127.0.0.1:{daemon_port}");
    let mut s = Session::start(&db, &daemon_url);
    s.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "zestroku-test", "version": "0"}
        }
    }));
    let init = s.response(1);
    assert_eq!(init["result"]["serverInfo"]["name"], "zestroku", "{init}");
    s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    s.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let list = s.response(2);
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for want in [
        "echo",
        "list_devices",
        "info",
        "active_app",
        "apps",
        "recent_actions",
        "key",
        "launch",
    ] {
        assert!(names.contains(&want), "missing tool {want}: {names:?}");
    }
    let devices_schema = &listed_tool(&list, "list_devices")["inputSchema"];
    assert_eq!(devices_schema["type"], "object");
    assert_eq!(devices_schema["additionalProperties"], false);
    assert_eq!(devices_schema["properties"], json!({}));
    assert!(
        !devices_schema.as_object().unwrap().contains_key("required"),
        "{devices_schema}"
    );
    for name in ["info", "active_app", "apps"] {
        let schema = &listed_tool(&list, name)["inputSchema"];
        assert_eq!(schema["type"], "object", "{name}");
        assert_eq!(
            schema["properties"],
            json!({
                "host": {
                    "type": "string",
                    "description": "Inventory host or host:port."
                }
            }),
            "{name}"
        );
        assert_eq!(schema["required"], json!(["host"]), "{name}");
        assert_eq!(schema["additionalProperties"], false, "{name}");
    }

    let echo = s.call(3, "echo", json!({"text": "hello \u{2014} roku"}));
    assert!(!is_error(&echo), "{echo}");
    assert_eq!(
        echo["result"]["content"][0]["text"], "hello \u{2014} roku",
        "{echo}"
    );

    let devices = s.call(4, "list_devices", json!({}));
    assert!(!is_error(&devices), "{devices}");
    let devices = result_json(&devices);
    let listed = devices
        .as_array()
        .expect("device inventory array")
        .iter()
        .find(|device| device["model_number"] == "SIM-1234X")
        .expect("seeded simulator in daemon inventory");
    assert_eq!(listed["host"], "127.0.0.1");
    assert_eq!(listed["access_tier"], "permissive");

    let info = s.call(5, "info", json!({"host": target}));
    assert!(!is_error(&info), "{info}");
    let info = result_json(&info);
    assert_eq!(info["model_name"], "Sim Express");
    assert_eq!(info["model_number"], "SIMX1");
    assert_eq!(info["software_version"], "15.2.4");
    assert_eq!(info["access_tier"], "permissive");
    assert_eq!(info["is_tv"], false);

    let active_app = s.call(6, "active_app", json!({"host": target}));
    assert!(!is_error(&active_app), "{active_app}");
    assert_eq!(result_json(&active_app)["name"], "Roku");

    let apps = s.call(7, "apps", json!({"host": target}));
    assert!(!is_error(&apps), "{apps}");
    let apps = result_json(&apps);
    let installed = apps["apps"].as_array().expect("installed apps array");
    assert!(
        installed
            .iter()
            .any(|app| app["id"] == "1001" && app["name"] == "Streaming One"),
        "{apps}"
    );
    let read_requests = sim.received_requests();
    assert!(
        read_requests.iter().all(|(method, _)| method == "GET"),
        "MCP read tools must not issue POST requests: {read_requests:?}"
    );

    for path in ["/query/device-info", "/query/active-app", "/query/apps"] {
        assert!(
            read_requests
                .iter()
                .any(|(method, request_uri)| method == "GET" && request_uri.as_str() == path),
            "expected daemon-routed GET {path}; observed {read_requests:?}"
        );
    }

    let before_unknown = sim.received_requests().len();
    let unknown = s.call(8, "info", json!({"host": "192.0.2.99:8060"}));
    assert!(
        tool_error_text(&unknown).contains("device_not_found"),
        "{unknown}"
    );
    assert_eq!(sim.received_requests().len(), before_unknown);

    let before_extra = sim.received_requests().len();
    let extra_backend = s.call(
        9,
        "info",
        json!({"host": target, "daemon_url": "http://192.0.2.1:8070"}),
    );
    assert_eq!(extra_backend["error"]["code"], -32602, "{extra_backend}");
    assert!(
        extra_backend["error"]["message"]
            .as_str()
            .is_some_and(|message| {
                message.contains("additional property not allowed: daemon_url")
            }),
        "{extra_backend}"
    );
    assert_eq!(sim.received_requests().len(), before_extra);

    sim.set_malformed_apps(true);
    let malformed = s.call(10, "apps", json!({"host": target}));
    assert!(
        tool_error_text(&malformed).contains("malformed_apps"),
        "{malformed}"
    );

    let unreachable = s.call(11, "info", json!({"host": unreachable_target}));
    assert!(
        tool_error_text(&unreachable).contains("transport_error"),
        "{unreachable}"
    );

    let requests = sim.received_requests();
    assert!(
        requests.iter().all(|(method, _)| method == "GET"),
        "MCP read tools must not issue POSTs: {requests:?}"
    );

    // Omitting `confirm` is refused by the tool's own schema before the gate.
    let missing = s.call(12, "key", json!({"host": "127.0.0.1", "target": "Home"}));
    assert!(
        is_error(&missing),
        "missing confirm must be refused: {missing}"
    );

    // An arbitrary token reaches the gate: refused AND audited.
    let refused = s.call(
        13,
        "key",
        json!({"host": target, "target": "Home", "confirm": "not-a-token"}),
    );
    assert!(
        is_error(&refused),
        "unconfirmed key must be refused: {refused}"
    );
    assert!(
        refused.to_string().contains("CONFIRMATION_REQUIRED"),
        "{refused}"
    );

    let audit = s.call(14, "recent_actions", json!({}));
    let audit_text = audit["result"]["content"][0]["text"].as_str().unwrap();
    assert!(audit_text.contains("denied"), "{audit_text}");

    let before_shutdown = sim.received_requests().len();
    daemon.stop();
    for (id, name, arguments) in [
        (15, "list_devices", json!({})),
        (16, "info", json!({"host": target})),
        (17, "active_app", json!({"host": target})),
        (18, "apps", json!({"host": target})),
    ] {
        let unavailable = s.call(id, name, arguments);
        assert!(
            tool_error_text(&unavailable).contains("daemon_unavailable"),
            "{name}: {unavailable}"
        );
    }
    assert_eq!(sim.received_requests().len(), before_shutdown);
}

#[test]
fn mcp_launch_and_key_dispatch_once_through_daemon_and_audit_state() {
    use zestroku_core::store::Store;
    use zestroku_proto::launch;
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let target = format!("127.0.0.1:{}", sim.port());
    let dir = tempfile::tempdir().expect("temporary mutation directory");
    let db = dir.path().join("actions.db");
    seed_mutation_device(&db, &target, "SIM-1234X");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64;
    let launch_token = mint_mcp_approval(&db, &target, &launch(1001), now);
    let wrong_case_token = mint_cli_approval(
        &db,
        &target,
        "HOME",
        &dir.path().join("wrong-case-approval.typescript"),
    );
    let key_token = mint_cli_approval(
        &db,
        &target,
        "Home",
        &dir.path().join("home-approval.typescript"),
    );

    let daemon_port = free_port();
    let _daemon = Daemon::start(&db, daemon_port);
    let mut session = Session::start(&db, &format!("http://127.0.0.1:{daemon_port}"));
    initialize_session(&mut session);

    let launched = session.call(
        2,
        "launch",
        json!({"host": target, "target": "1001", "confirm": launch_token}),
    );
    assert!(!is_error(&launched), "{launched}");
    assert_eq!(
        sim.launch_count(),
        1,
        "MCP approval must reach the daemon and dispatch exactly once"
    );
    let launched_outcome = result_json(&launched);
    assert_eq!(launched_outcome["surface"], "launch");
    assert_eq!(launched_outcome["target"], target);
    assert_eq!(launched_outcome["outcome"], "completed");

    let active_after_launch = session.call(3, "active_app", json!({"host": target}));
    assert_eq!(
        result_json(&active_after_launch)["name"],
        "Streaming One",
        "{active_after_launch}"
    );

    let wrong_case = session.call(
        4,
        "key",
        json!({"host": target, "target": "Home", "confirm": wrong_case_token}),
    );
    assert!(
        is_error(&wrong_case),
        "HOME-bound token must not authorize the Home wire path: {wrong_case}"
    );
    let posts_after_wrong_case: Vec<String> = sim
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .map(|(_, path)| path)
        .collect();
    assert_eq!(posts_after_wrong_case, ["/launch/1001"]);

    let keyed = session.call(
        5,
        "key",
        json!({"host": target, "target": "Home", "confirm": key_token}),
    );
    assert!(!is_error(&keyed), "{keyed}");
    let keyed_outcome = result_json(&keyed);
    assert_eq!(keyed_outcome["surface"], "key");
    assert_eq!(keyed_outcome["target"], target);
    assert_eq!(keyed_outcome["outcome"], "completed");

    let active_after_key = session.call(6, "active_app", json!({"host": target}));

    assert_eq!(
        result_json(&active_after_key)["name"],
        "Roku",
        "{active_after_key}"
    );
    let posts: Vec<String> = sim
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .map(|(_, path)| path)
        .collect();
    assert_eq!(posts, ["/launch/1001", "/keypress/Home"]);

    let actions = SqliteStore::open(&db)
        .expect("open shared action store")
        .list_actions()
        .expect("list shared actions");
    assert_eq!(actions.len(), 3, "{actions:?}");
    let launch_action = actions
        .iter()
        .find(|action| action.surface == "launch")
        .expect("completed launch action");
    assert_eq!(launch_action.client, "http");
    assert_eq!(launch_action.decision, "approved");
    assert_eq!(launch_action.result, "completed");
    assert!(launch_action.before_state.contains("Roku"));
    let key_denial = actions
        .iter()
        .find(|action| action.surface == "key" && action.decision == "denied")
        .expect("case-mismatched key token is denied");
    assert_eq!(
        key_denial.result,
        zestroku_core::mutation::CONFIRMATION_INVALID
    );
    let key_action = actions
        .iter()
        .find(|action| action.surface == "key" && action.decision == "approved")
        .expect("completed key action");
    assert_eq!(key_action.client, "http");
    assert_eq!(key_action.result, "completed");
    assert!(key_action.before_state.contains("Streaming One"));
}

#[test]
fn mcp_replay_is_refused_without_a_second_device_mutation() {
    use zestroku_core::store::Store;
    use zestroku_proto::launch;
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let target = format!("127.0.0.1:{}", sim.port());
    let dir = tempfile::tempdir().expect("temporary replay directory");
    let db = dir.path().join("actions.db");
    seed_mutation_device(&db, &target, "SIM-1234X");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64;
    let token = mint_mcp_approval(&db, &target, &launch(1001), now);

    let daemon_port = free_port();
    let _daemon = Daemon::start(&db, daemon_port);
    let mut session = Session::start(&db, &format!("http://127.0.0.1:{daemon_port}"));
    initialize_session(&mut session);
    let arguments = json!({"host": target, "target": "1001", "confirm": token});

    let first = session.call(2, "launch", arguments.clone());
    assert!(!is_error(&first), "{first}");
    assert_eq!(sim.launch_count(), 1);
    let replay = session.call(3, "launch", arguments);
    assert!(
        is_error(&replay),
        "replayed approval must be refused: {replay}"
    );
    assert_eq!(
        sim.launch_count(),
        1,
        "replayed token must not dispatch another mutation"
    );
    let posts: Vec<String> = sim
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .map(|(_, path)| path)
        .collect();
    assert_eq!(posts, ["/launch/1001"]);

    let actions = SqliteStore::open(&db)
        .expect("open shared action store")
        .list_actions()
        .expect("list shared actions");
    assert_eq!(actions.len(), 2, "{actions:?}");
    assert_eq!(actions[0].result, "completed");
    assert_eq!(actions[1].decision, "denied");
}

#[test]
fn mcp_key_and_launch_daemon_down_preserves_tokens_for_restart() {
    use zestroku_proto::{keypress_raw, launch};

    let sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let target = format!("127.0.0.1:{}", sim.port());
    let dir = tempfile::tempdir().expect("temporary daemon-down directory");
    let db = dir.path().join("actions.db");
    seed_mutation_device(&db, &target, "SIM-1234X");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64;
    let key_token = mint_mcp_approval(&db, &target, &keypress_raw("Home"), now);
    let launch_token = mint_mcp_approval(&db, &target, &launch(1001), now);

    let daemon_port = free_port();
    let mut daemon = Daemon::start(&db, daemon_port);
    let mut session = Session::start(&db, &format!("http://127.0.0.1:{daemon_port}"));
    initialize_session(&mut session);
    let baseline_posts = sim
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .count();
    daemon.stop();
    let key_down = session.call(
        2,
        "key",
        json!({"host": target, "target": "Home", "confirm": key_token}),
    );
    assert!(
        tool_error_text(&key_down).contains("daemon_unavailable"),
        "{key_down}"
    );
    let launch_down = session.call(
        3,
        "launch",
        json!({"host": target, "target": "1001", "confirm": launch_token}),
    );
    assert!(
        tool_error_text(&launch_down).contains("daemon_unavailable"),
        "{launch_down}"
    );
    let down_posts: Vec<String> = sim
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .map(|(_, path)| path)
        .collect();
    assert_eq!(
        down_posts.len(),
        baseline_posts,
        "daemon-down mutations must not reach the device: {down_posts:?}"
    );
    daemon = Daemon::start(&db, daemon_port);
    let keyed = session.call(
        4,
        "key",
        json!({"host": target, "target": "Home", "confirm": key_token}),
    );
    assert!(!is_error(&keyed), "{keyed}");
    assert_eq!(result_json(&keyed)["outcome"], "completed");
    let launched = session.call(
        5,
        "launch",
        json!({"host": target, "target": "1001", "confirm": launch_token}),
    );
    assert!(!is_error(&launched), "{launched}");
    assert_eq!(result_json(&launched)["outcome"], "completed");
    let posts: Vec<String> = sim
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .map(|(_, path)| path)
        .collect();
    assert_eq!(posts[baseline_posts..], ["/keypress/Home", "/launch/1001"]);
    daemon.stop();
}
#[test]
fn mcp_rejects_non_loopback_daemon_endpoint_before_startup() {
    let output = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .arg("mcp")
        .env("ZESTROKU_DAEMON_URL", "http://198.51.100.7:8070")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run MCP with an out-of-scope daemon endpoint");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("ZESTROKU_DAEMON_URL must be http://<loopback-ip>:<nonzero-port>"),
        "invalid endpoint must be rejected before the MCP server starts"
    );
    assert!(
        output.stdout.is_empty(),
        "invalid endpoint must not emit an MCP handshake"
    );
}

#[test]
fn mcp_read_consumer_rejects_daemon_schema_drift() {
    let valid = json!([{
        "name": "Sim Roku",
        "host": "127.0.0.1",
        "model": "Roku Express",
        "model_number": "SIMX1",
        "software_version": "15.2.4",
        "access_tier": "permissive",
        "last_seen": 1
    }]);
    let mut drifted = valid.clone();
    drifted[0]["unexpected"] = json!(true);
    let responses = [valid.to_string(), drifted.to_string()];

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind malformed daemon");
    let address = listener.local_addr().expect("malformed daemon address");
    let daemon = thread::spawn(move || {
        for body in responses {
            let (stream, _) = listener.accept().expect("MCP read request");
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("read request line");
            let mut parts = request_line.split_whitespace();
            assert_eq!(parts.next(), Some("GET"));
            assert_eq!(parts.next(), Some("/v1/devices"));

            let mut stream = reader.into_inner();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("write schema response");
            stream.flush().expect("flush schema response");
        }
    });

    let dir = tempfile::tempdir().expect("temporary MCP schema directory");
    let db = dir.path().join("actions.db");
    let mut session = Session::start(&db, &format!("http://{address}"));
    initialize_session(&mut session);

    let valid_response = session.call(2, "list_devices", json!({}));
    assert!(!is_error(&valid_response), "{valid_response}");
    let devices = result_json(&valid_response);
    assert_eq!(devices[0]["name"], "Sim Roku");
    assert_eq!(devices[0]["model_number"], "SIMX1");

    let drifted_response = session.call(3, "list_devices", json!({}));
    assert!(
        tool_error_text(&drifted_response).contains("daemon_response_schema_mismatch"),
        "{drifted_response}"
    );
    daemon.join().expect("schema-drift daemon");
}
