#![forbid(unsafe_code)]

//! `zestroku mcp` speaks MCP over stdio: spawns the real binary, initializes,
//! lists tools, round-trips `echo`, and reads `list_devices`, `info`,
//! `active_app`, and `apps` through the real daemon and simulator. It checks
//! unavailable-daemon behavior without fallback and proves an unconfirmed
//! `key` is refused with CONFIRMATION_REQUIRED and audited.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use zestroku_core::mutation::ClientPrincipal;
use zestroku_core::store::{DeviceRow, SqliteStore, Store};

#[path = "mcp_stdio/principal_policy.rs"]
mod principal_policy;

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
        let targets = db.with_extension("targets.toml");
        std::fs::write(&targets, "").expect("write empty test targets");
        let child = Command::new(env!("CARGO_BIN_EXE_zestroku"))
            .env("ZESTROKU_DB", db)
            .args(["serve", "--bind"])
            .arg(format!("127.0.0.1:{port}"))
            .args(["--no-ssdp", "--targets"])
            .arg(&targets)
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

fn mint_approval(
    db: &Path,
    target: &str,
    command: &zestroku_proto::Command,
    issued_at: i64,
    client: ClientPrincipal,
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
        client,
        issued_at,
    )
    .expect("mint one-use client-bound test approval")
}
/// Mint a real CLI approval through `script` so the terminal-only CLI gate
/// stays active in this process-level regression.
fn mint_cli_approval(
    db: &Path,
    target: &str,
    key: &str,
    requested_client: Option<zestroku_core::mutation::ClientPrincipal>,
    transcript: &Path,
) -> String {
    let binary = env!("CARGO_BIN_EXE_zestroku");
    let effective_client =
        requested_client.unwrap_or(zestroku_core::mutation::ClientPrincipal::Cli);
    let mut command = Command::new("script");
    #[cfg(target_os = "macos")]
    {
        command
            .args(["-q", "-e"])
            .arg(transcript)
            .arg(binary)
            .args(["approve", "key", target, key]);
        if let Some(client) = requested_client {
            command.args(["--client", client.as_str()]);
        }
    }
    #[cfg(target_os = "linux")]
    {
        let command_line = match requested_client {
            Some(client) => format!(
                "{binary} approve key {target} {key} --client {}",
                client.as_str()
            ),
            None => format!("{binary} approve key {target} {key}"),
        };
        command
            .args(["-q", "-e", "-c"])
            .arg(command_line)
            .arg(transcript);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    panic!("CLI approval PTY test requires macOS or Linux `script`");

    let mut child = command
        .env("ZESTROKU_DB", db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start CLI approval under PTY");
    let phrase = format!(
        "approve key {target} {key} --client {}\n",
        effective_client.as_str()
    );
    // BSD script injects EOF into the PTY if its input closes before approval.
    let mut input = child.stdin.take().expect("PTY stdin");
    input
        .write_all(phrase.as_bytes())
        .expect("write exact approval phrase");
    let output = child.wait_with_output().expect("wait for CLI approval");
    drop(input);
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
    let malformed_sim = zestroku_sim::SimDevice::start(zestroku_sim::SimConfig {
        kind: zestroku_sim::DeviceKind::Express,
        tier: zestroku_sim::SimTier::Permissive,
    });
    let malformed_target = format!("127.0.0.1:{}", malformed_sim.port());
    malformed_sim.set_malformed_apps(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    seed_mutation_device(&db, &target, "SIM-1234X");
    seed_device(&db, &unreachable_target, "SIM-DEAD");
    seed_device(&db, &malformed_target, "SIM-MALFORMED");
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
        "get_device_state",
        "recent_actions",
        "key",
        "launch",
        "undo_last",
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
    for name in ["info", "active_app", "apps", "get_device_state"] {
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

    s.send(&json!({"jsonrpc": "2.0", "id": 22, "method": "resources/list", "params": {}}));
    let resources = s.response(22);
    assert!(
        resources["result"]["resources"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["uri"] == "roku://devices")),
        "{resources}"
    );
    s.send(&json!({
        "jsonrpc": "2.0", "id": 23, "method": "resources/templates/list", "params": {}
    }));
    let templates = s.response(23);
    assert!(
        templates["result"]["resourceTemplates"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["uriTemplate"] == "roku://devices/{id}")
            }),
        "{templates}"
    );

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

    let mut read_id = 30_u64;
    let deadline = Instant::now() + Duration::from_secs(15); // three 5-second poll-loop ticks
    let initial_state = loop {
        let state = s.call(read_id, "get_device_state", json!({"host": target}));
        read_id += 1;
        if !is_error(&state) {
            let result = result_json(&state);
            if result["fresh"] == true {
                break result;
            }
        }
        assert!(
            Instant::now() < deadline,
            "daemon poller did not produce a fresh device-state snapshot"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(initial_state["model_number"], "SIMX1");
    assert_eq!(initial_state["active_app_name"], "Roku");
    assert_eq!(initial_state["apps_status"], "available");
    assert_eq!(initial_state["media_player_status"], "available");
    let before_cached_reads = sim.received_requests().len();

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

    let state = s.call(read_id, "get_device_state", json!({"host": target}));
    read_id += 1;
    assert!(!is_error(&state), "{state}");
    assert_eq!(result_json(&state)["fresh"], true);

    s.send(&json!({
        "jsonrpc": "2.0", "id": read_id, "method": "resources/read",
        "params": {"uri": "roku://devices"}
    }));
    let inventory_resource = s.response(read_id);
    read_id += 1;
    let inventory_text = inventory_resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("inventory resource JSON text");
    let inventory_json: Value =
        serde_json::from_str(inventory_text).expect("inventory resource JSON");
    assert!(
        inventory_json
            .as_array()
            .unwrap()
            .iter()
            .any(|device| device["model_number"] == "SIM-1234X"),
        "{inventory_json}"
    );

    let state_uri = format!("roku://devices/{target}");
    s.send(&json!({
        "jsonrpc": "2.0", "id": read_id, "method": "resources/read",
        "params": {"uri": state_uri}
    }));
    let state_resource = s.response(read_id);
    let state_text = state_resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("device-state resource JSON text");
    let state_json: Value = serde_json::from_str(state_text).expect("device-state resource JSON");
    assert_eq!(state_json["model_number"], "SIMX1");
    assert_eq!(state_json["fresh"], true);
    assert_eq!(
        sim.received_requests().len(),
        before_cached_reads,
        "fresh daemon-backed reads and resources must not issue ECP round trips"
    );

    let read_requests = sim.received_requests();
    assert!(
        read_requests.iter().all(|(method, _)| method == "GET"),
        "MCP reads must not issue POST requests: {read_requests:?}"
    );
    for path in [
        "/query/device-info",
        "/query/active-app",
        "/query/apps",
        "/query/media-player",
        "/query/tv-active-channel",
    ] {
        assert!(
            read_requests
                .iter()
                .any(|(method, request_uri)| method == "GET" && request_uri.as_str() == path),
            "expected daemon poll GET {path}; observed {read_requests:?}"
        );
    }
    let before_unknown = sim.received_requests().len();
    let unknown = s.call(8, "info", json!({"host": "192.0.2.99:8060"}));
    let unknown_error: Value =
        serde_json::from_str(tool_error_text(&unknown)).expect("structured unknown-device JSON");
    assert_eq!(unknown_error["error"], "device_not_found");
    assert_eq!(unknown_error["code"], "UNKNOWN_DEVICE");
    assert_eq!(
        unknown_error["hint"],
        "name a known device id, alias, or host:port (see `devices`)"
    );
    assert!(unknown_error["suggestions"].as_array().unwrap().is_empty());
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

    let malformed = s.call(10, "apps", json!({"host": malformed_target}));
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
    let before_missing = sim.received_requests().len();
    let missing = s.call(12, "key", json!({"host": target, "target": "Home"}));
    assert!(
        is_error(&missing),
        "missing confirm must be refused: {missing}"
    );
    assert_eq!(sim.received_requests().len(), before_missing);

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
    let audit_text = audit["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(audit_text.contains("denied"), "{audit_text}");
    assert!(audit_text.contains("client=unknown"), "{audit_text}");
    assert!(audit_text.contains("surface=key"), "{audit_text}");
    assert!(
        audit_text.contains("result=\"CONFIRMATION_INVALID\""),
        "{audit_text}"
    );
    assert!(audit_text.contains("before="), "{audit_text}");
    let before_invalid_launch = sim.received_requests().len();
    let invalid_launch = s.call(
        24,
        "launch",
        json!({"host": target, "target": "netflix", "confirm": "unused-token"}),
    );
    assert!(is_error(&invalid_launch), "{invalid_launch}");
    assert!(
        tool_error_text(&invalid_launch).contains("app id must be a number"),
        "{invalid_launch}"
    );
    let non_string_confirm = s.call(
        25,
        "key",
        json!({"host": target, "target": "Home", "confirm": true}),
    );
    assert!(is_error(&non_string_confirm), "{non_string_confirm}");
    let after_invalid_arguments = s.call(26, "recent_actions", json!({}));
    let after_invalid_text = after_invalid_arguments["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(after_invalid_text, audit_text);
    assert_eq!(sim.received_requests().len(), before_invalid_launch);
    let before_whitespace = sim.received_requests();
    let whitespace_token = s.call(
        27,
        "key",
        json!({"host": target, "target": "Home", "confirm": "   "}),
    );
    assert!(is_error(&whitespace_token), "{whitespace_token}");
    assert!(
        tool_error_text(&whitespace_token).contains("CONFIRMATION_REQUIRED"),
        "{whitespace_token}"
    );
    let whitespace_audit = s.call(28, "recent_actions", json!({}));
    let whitespace_audit_text = whitespace_audit["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(
        whitespace_audit_text.matches("decision=denied").count(),
        audit_text.matches("decision=denied").count() + 1,
        "{whitespace_audit_text}"
    );
    let after_whitespace = sim.received_requests();
    assert!(
        after_whitespace.len() >= before_whitespace.len(),
        "the before-state read must remain observable: {after_whitespace:?}"
    );
    let new_requests = &after_whitespace[before_whitespace.len()..];
    assert!(
        new_requests.iter().all(|(method, _)| method == "GET"),
        "a refused whitespace token must not dispatch an ECP mutation: {new_requests:?}"
    );
    assert!(
        new_requests
            .iter()
            .any(|(method, path)| method == "GET" && path == "/query/active-app"),
        "mutation audit must retain the before-state read: {new_requests:?}"
    );
    let before_shutdown = sim.received_requests().len();
    daemon.stop();
    for (id, name, arguments) in [
        (15, "list_devices", json!({})),
        (16, "info", json!({"host": target})),
        (17, "active_app", json!({"host": target})),
        (18, "apps", json!({"host": target})),
        (19, "get_device_state", json!({"host": target})),
    ] {
        let unavailable = s.call(id, name, arguments);
        let error: Value = serde_json::from_str(tool_error_text(&unavailable))
            .expect("daemon-unavailable semantic error JSON");
        assert_eq!(
            error["error"], "daemon_unavailable",
            "{name}: {unavailable}"
        );
        assert_eq!(error["code"], "DAEMON_UNAVAILABLE", "{name}: {unavailable}");
        assert_eq!(
            error["hint"], "start the daemon or pass --direct for diagnosis",
            "{name}: {unavailable}"
        );
    }
    for (id, uri) in [
        (20, "roku://devices".to_owned()),
        (21, format!("roku://devices/{target}")),
    ] {
        s.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "resources/read",
            "params": {"uri": uri}
        }));
        let unavailable = s.response(id);
        assert!(is_error(&unavailable), "{unavailable}");
        assert!(
            unavailable.to_string().contains("DAEMON_UNAVAILABLE"),
            "{unavailable}"
        );
    }
    assert_eq!(sim.received_requests().len(), before_shutdown);
}

#[test]
fn unknown_device_error_metadata_matches_http_mcp_and_cli() {
    let directory = tempfile::tempdir().expect("empty-daemon tempdir");
    let daemon_db = directory.path().join("daemon.db");
    let client_db = directory.path().join("client.db");
    let targets = directory.path().join("targets.toml");
    std::fs::write(&targets, "").expect("write empty targets file");
    let port = free_port();
    let daemon_host = format!("127.0.0.1:{port}");
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_zestroku"))
            .env("ZESTROKU_DB", &daemon_db)
            .args(["serve", "--bind", &daemon_host, "--no-ssdp", "--targets"])
            .arg(&targets)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn empty-inventory daemon"),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = daemon.0.try_wait().expect("poll daemon process") {
            panic!("serve exited before readiness: {status}");
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "empty-inventory daemon did not listen"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let daemon_url = format!("http://{daemon_host}");
    let unknown_target = "192.0.2.99:8060";

    let mut session = Session::start(&client_db, &daemon_url);
    session.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "zestroku-test", "version": "0"}
        }
    }));
    let initialized = session.response(1);
    assert_eq!(initialized["result"]["serverInfo"]["name"], "zestroku");
    session.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let devices = session.call(2, "list_devices", json!({}));
    assert!(!is_error(&devices), "{devices}");
    assert_eq!(
        result_json(&devices),
        json!([]),
        "daemon inventory must be empty"
    );

    let path = format!("/v1/devices/{unknown_target}/info");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect HTTP client");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set HTTP read timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {daemon_host}\r\nConnection: close\r\n\r\n"
    )
    .expect("write HTTP read");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read HTTP response");
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .expect("HTTP response has a header/body separator");
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .expect("HTTP status");
    assert_eq!(status, 404, "{response}");
    let http_error: Value = serde_json::from_str(body).expect("HTTP semantic error JSON");

    let mcp_response = session.call(3, "info", json!({"host": unknown_target}));
    let mcp_error: Value =
        serde_json::from_str(tool_error_text(&mcp_response)).expect("MCP semantic error JSON");
    for field in ["error", "code", "detail", "hint", "suggestions"] {
        assert_eq!(http_error[field], mcp_error[field], "{field}");
    }
    assert_eq!(http_error["error"], "device_not_found");
    assert_eq!(http_error["code"], "UNKNOWN_DEVICE");

    let cli = Command::new(env!("CARGO_BIN_EXE_zestroku"))
        .env("ZESTROKU_DB", &client_db)
        .env("ZESTROKU_DAEMON_URL", &daemon_url)
        .env_remove("ZESTROKU_DIRECT")
        .env_remove("ZESTROKU_TIER")
        .env_remove("ZESTROKU_TRANSPORT")
        .args(["info", unknown_target, "--json"])
        .output()
        .expect("run real daemon-backed CLI read");
    assert_eq!(cli.status.code(), Some(1), "{cli:?}");
    assert!(cli.stdout.is_empty(), "{cli:?}");
    let cli_stderr = String::from_utf8_lossy(&cli.stderr);
    let code = http_error["code"].as_str().expect("semantic code");
    let error = http_error["error"].as_str().expect("operation error");
    let detail = http_error["detail"].as_str().expect("semantic detail");
    let hint = http_error["hint"].as_str().expect("semantic hint");
    let suggestions: Vec<String> = http_error["suggestions"]
        .as_array()
        .expect("suggestions array")
        .iter()
        .map(|suggestion| suggestion.as_str().expect("suggestion string").to_owned())
        .collect();
    assert!(cli_stderr.contains(code), "{cli_stderr}");
    assert!(cli_stderr.contains(error), "{cli_stderr}");
    assert!(cli_stderr.contains(detail), "{cli_stderr}");
    assert!(
        cli_stderr.contains(&format!("hint: {hint}")),
        "{cli_stderr}"
    );
    assert!(
        cli_stderr.contains(&format!("suggestions: {suggestions:?}")),
        "{cli_stderr}"
    );
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
    let launch_token = mint_approval(&db, &target, &launch(1001), now, ClientPrincipal::McpStdio);
    let wrong_case_token = mint_cli_approval(
        &db,
        &target,
        "HOME",
        None,
        &dir.path().join("wrong-case-approval.typescript"),
    );
    let cli_bound_key_token = mint_cli_approval(
        &db,
        &target,
        "Home",
        None,
        &dir.path().join("cli-home-approval.typescript"),
    );
    let mcp_key_token = mint_cli_approval(
        &db,
        &target,
        "Home",
        Some(zestroku_core::mutation::ClientPrincipal::McpStdio),
        &dir.path().join("mcp-home-approval.typescript"),
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

    let wrong_client = session.call(
        5,
        "key",
        json!({"host": target, "target": "Home", "confirm": cli_bound_key_token}),
    );
    assert!(
        is_error(&wrong_client),
        "an MCP request must not relabel a CLI-bound grant: {wrong_client}"
    );
    let posts_after_wrong_client: Vec<String> = sim
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .map(|(_, path)| path)
        .collect();
    assert_eq!(posts_after_wrong_client, ["/launch/1001"]);

    let keyed = session.call(
        6,
        "key",
        json!({"host": target, "target": "Home", "confirm": mcp_key_token}),
    );
    assert!(!is_error(&keyed), "{keyed}");
    let keyed_outcome = result_json(&keyed);
    assert_eq!(keyed_outcome["surface"], "key");
    assert_eq!(keyed_outcome["target"], target);
    assert_eq!(keyed_outcome["outcome"], "completed");

    let active_after_key = session.call(7, "active_app", json!({"host": target}));

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
    assert_eq!(actions.len(), 4, "{actions:?}");
    let launch_action = actions
        .iter()
        .find(|action| action.surface == "launch")
        .expect("completed launch action");
    assert_eq!(launch_action.client, "mcp-stdio");
    assert_eq!(launch_action.decision, "approved");
    assert_eq!(launch_action.result, "completed");
    assert!(launch_action.before_state.contains("Roku"));
    let key_denials: Vec<_> = actions
        .iter()
        .filter(|action| action.surface == "key" && action.decision == "denied")
        .collect();
    assert_eq!(key_denials.len(), 2, "{actions:?}");
    for denial in key_denials {
        assert_eq!(denial.client, "unknown");
        assert_eq!(denial.result, zestroku_core::mutation::CONFIRMATION_INVALID);
    }
    let key_action = actions
        .iter()
        .find(|action| action.surface == "key" && action.decision == "approved")
        .expect("completed key action");
    assert_eq!(key_action.client, "mcp-stdio");
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
    let token = mint_approval(&db, &target, &launch(1001), now, ClientPrincipal::McpStdio);

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
    let key_token = mint_approval(
        &db,
        &target,
        &keypress_raw("Home"),
        now,
        ClientPrincipal::McpStdio,
    );
    let launch_token = mint_approval(&db, &target, &launch(1001), now, ClientPrincipal::McpStdio);

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
fn mcp_undo_last_uses_launch_confirmation_and_records_the_restore() {
    use zestroku_core::store::Store;
    use zestroku_proto::launch;
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let target = format!("127.0.0.1:{}", sim.port());
    let dir = tempfile::tempdir().expect("temporary undo directory");
    let db = dir.path().join("actions.db");
    seed_mutation_device(&db, &target, "SIM-1234X");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64;
    let first_token = mint_approval(&db, &target, &launch(1001), now, ClientPrincipal::McpStdio);
    let second_token = mint_approval(&db, &target, &launch(1002), now, ClientPrincipal::McpStdio);
    let undo_token = mint_approval(&db, &target, &launch(1001), now, ClientPrincipal::McpStdio);

    let daemon_port = free_port();
    let _daemon = Daemon::start(&db, daemon_port);
    let mut session = Session::start(&db, &format!("http://127.0.0.1:{daemon_port}"));
    initialize_session(&mut session);
    for (id, token, app_id) in [(2, first_token, 1001), (3, second_token, 1002)] {
        let launch = session.call(
            id,
            "launch",
            json!({"host": target, "target": app_id.to_string(), "confirm": token}),
        );
        assert!(!is_error(&launch), "{launch}");
    }
    let denied = session.call(4, "undo_last", json!({"host": target}));
    assert!(
        is_error(&denied),
        "undo without confirmation must be refused: {denied}"
    );
    let undone = session.call(
        5,
        "undo_last",
        json!({"host": target, "confirm": undo_token}),
    );
    assert!(!is_error(&undone), "{undone}");
    assert_eq!(result_json(&undone)["surface"], "undo");
    assert_eq!(result_json(&undone)["outcome"], "completed");
    assert_eq!(sim.launch_count(), 3);

    let posts: Vec<String> = sim
        .received_requests()
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .map(|(_, path)| path)
        .collect();
    assert_eq!(posts, ["/launch/1001", "/launch/1002", "/launch/1001"]);
    let actions = SqliteStore::open(&db)
        .expect("open undo action store")
        .list_actions()
        .expect("read undo action store");
    assert_eq!(actions[0].client, "mcp-stdio");
    assert_eq!(actions[1].client, "mcp-stdio");
    assert_eq!(actions[2].client, "unknown");
    assert_eq!(actions[3].client, "mcp-stdio");
    assert_eq!(actions[2].surface, "undo");
    assert_eq!(actions[2].decision, "denied");
    assert_eq!(actions[3].surface, "undo");
    assert_eq!(actions[3].decision, "approved");
    assert_eq!(actions[3].result, "completed");
    assert!(actions[3].intent.contains("Streaming One"));
    assert!(actions[3].before_state.contains("Streaming Two"));
    let audit = session.call(6, "recent_actions", json!({}));
    assert!(!is_error(&audit), "{audit}");
    let audit_text = audit["result"]["content"][0]["text"]
        .as_str()
        .expect("action log text");
    assert!(audit_text.contains("surface=undo"), "{audit_text}");
    assert!(audit_text.contains("decision=denied"), "{audit_text}");
    assert!(audit_text.contains("decision=approved"), "{audit_text}");
}

#[test]
fn mcp_preserves_missing_app_undo_plan_error_without_post() {
    use zestroku_core::store::ActionRow;
    use zestroku_core::store::Store;
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Permissive,
    });
    let target = format!("127.0.0.1:{}", sim.port());
    let dir = tempfile::tempdir().expect("temporary missing-app directory");
    let db = dir.path().join("actions.db");
    seed_mutation_device(&db, &target, "SIM-1234X");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64;
    let before_state =
        zestroku_core::tier::marked_before_app_state("fixture", &target, Some(9999), "Removed App");
    SqliteStore::open(&db)
        .expect("open missing-app action store")
        .insert_action(&ActionRow {
            id: 0,
            at: now,
            client: "mcp-stdio".to_owned(),
            surface: "launch".to_owned(),
            intent: format!("launch {target}"),
            decision: "approved".to_owned(),
            result: "completed".to_owned(),
            before_state,
        })
        .expect("seed completed action for an absent prior app");

    let daemon_port = free_port();
    let _daemon = Daemon::start(&db, daemon_port);
    let mut session = Session::start(&db, &format!("http://127.0.0.1:{daemon_port}"));
    initialize_session(&mut session);

    let missing_app = session.call(2, "undo_last", json!({"host": target}));
    assert!(is_error(&missing_app), "{missing_app}");
    let error: Value =
        serde_json::from_str(tool_error_text(&missing_app)).expect("structured tool error JSON");
    assert_eq!(error["error"], "undo_app_not_installed");
    assert_eq!(error["code"], "APP_NOT_INSTALLED");
    assert_eq!(error["hint"], "use an installed app id from `apps`");
    assert!(error["suggestions"].as_array().unwrap().is_empty());
    assert_eq!(sim.launch_count(), 0);
    assert!(
        sim.received_requests()
            .iter()
            .all(|(method, _)| method == "GET"),
        "a missing-app undo plan must not issue a mutation: {:?}",
        sim.received_requests()
    );
}

#[test]
fn mcp_limited_tier_refusal_is_audited_without_device_post() {
    use zestroku_sim::{DeviceKind, SimConfig, SimDevice, SimTier};

    let sim = SimDevice::start(SimConfig {
        kind: DeviceKind::Express,
        tier: SimTier::Limited,
    });
    let target = format!("127.0.0.1:{}", sim.port());
    let dir = tempfile::tempdir().expect("temporary limited-tier directory");
    let db = dir.path().join("actions.db");
    seed_device(&db, &target, "SIM-LIMITED");

    let daemon_port = free_port();
    let _daemon = Daemon::start(&db, daemon_port);
    let mut session = Session::start(&db, &format!("http://127.0.0.1:{daemon_port}"));
    initialize_session(&mut session);

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut id = 2_u64;
    loop {
        let state = session.call(id, "get_device_state", json!({"host": target}));
        id += 1;
        if !is_error(&state) {
            let state = result_json(&state);
            if state["fresh"] == true && state["access_tier"] == "limited" {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not publish the Limited-tier poll snapshot"
        );
        thread::sleep(Duration::from_millis(10));
    }

    let requests_before = sim.received_requests();
    let refused = session.call(
        id,
        "key",
        json!({"host": target, "target": "Home", "confirm": "not-a-token"}),
    );
    assert!(is_error(&refused), "{refused}");
    let error: Value =
        serde_json::from_str(tool_error_text(&refused)).expect("structured tier error JSON");
    assert_eq!(error["error"], "tier_refused");
    assert_eq!(error["code"], "TIER_REFUSED");
    assert_eq!(error["hint"], "this operation needs a Permissive device");
    let requests_after = sim.received_requests();
    assert!(requests_after.len() >= requests_before.len());
    let new_requests = &requests_after[requests_before.len()..];
    assert!(
        new_requests.iter().all(|(method, _)| method == "GET"),
        "Limited-tier refusal must not dispatch a device mutation: {new_requests:?}"
    );
    assert!(
        new_requests
            .iter()
            .any(|(method, path)| method == "GET" && path == "/query/active-app"),
        "the daemon must keep the audited before-state snapshot: {new_requests:?}"
    );
    assert_eq!(sim.launch_count(), 0);

    let rows = SqliteStore::open(&db)
        .expect("open limited-tier action store")
        .list_actions()
        .expect("read limited-tier action store");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].client, "unknown");
    assert_eq!(rows[0].surface, "key");
    assert_eq!(rows[0].decision, "denied");
    assert_eq!(rows[0].result, "TIER_REFUSED");
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
