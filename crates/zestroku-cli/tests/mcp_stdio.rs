//! `zestroku mcp` speaks MCP over stdio: spawns the real binary, initializes,
//! lists tools, round-trips `echo`, reads a seeded device through
//! `list_devices`, and proves an unconfirmed `key` is refused with
//! CONFIRMATION_REQUIRED and audited (planted negative: a server that
//! dispatched without the gate would not refuse).

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;
use zestroku_core::store::{DeviceRow, SqliteStore, Store};

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Session {
    fn start(db: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zestroku"))
            .arg("mcp")
            .env("ZESTROKU_DB", db)
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

fn is_error(resp: &Value) -> bool {
    resp["error"].is_object() || resp["result"]["isError"] == true
}

#[test]
fn mcp_stdio_handshake_tools_and_gate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("actions.db");
    {
        let store = SqliteStore::open(&db).expect("seed store");
        store
            .upsert_device(&DeviceRow {
                id: "127.0.0.1:8060".into(),
                name: String::new(),
                ip: "127.0.0.1".into(),
                model: String::new(),
                model_num: "SIM-1234X".into(),
                firmware: "15.3.4".into(),
                wifi_driver: String::new(),
                access_tier: "permissive".into(),
                last_seen: 1,
            })
            .expect("seed device");
    }

    let mut s = Session::start(&db);
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
        .filter_map(|t| t["name"].as_str())
        .collect();
    for want in ["echo", "list_devices", "recent_actions", "key", "launch"] {
        assert!(names.contains(&want), "missing tool {want}: {names:?}");
    }

    let echo = s.call(3, "echo", json!({"text": "hello \u{2014} roku"}));
    assert!(!is_error(&echo), "{echo}");
    assert_eq!(
        echo["result"]["content"][0]["text"], "hello \u{2014} roku",
        "{echo}"
    );

    let devices = s.call(4, "list_devices", json!({}));
    assert!(!is_error(&devices), "{devices}");
    let text = devices["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("127.0.0.1:8060"), "{text}");
    assert!(text.contains("SIM-1234X"), "{text}");
    assert!(text.contains("tier=permissive"), "{text}");

    // Omitting `confirm` is refused by the tool's own schema before the gate.
    let missing = s.call(7, "key", json!({"host": "127.0.0.1", "target": "Home"}));
    assert!(
        is_error(&missing),
        "missing confirm must be refused: {missing}"
    );

    // An arbitrary token reaches the gate: refused AND audited.
    let refused = s.call(
        5,
        "key",
        json!({"host": "127.0.0.1", "target": "Home", "confirm": "not-a-token"}),
    );
    assert!(
        is_error(&refused),
        "unconfirmed key must be refused: {refused}"
    );
    assert!(
        refused.to_string().contains("CONFIRMATION_REQUIRED"),
        "{refused}"
    );

    let audit = s.call(6, "recent_actions", json!({}));
    let audit_text = audit["result"]["content"][0]["text"].as_str().unwrap();
    assert!(audit_text.contains("denied"), "{audit_text}");
}
