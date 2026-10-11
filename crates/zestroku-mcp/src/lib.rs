//! zestroku-mcp — MCP server tools over the same core gates as the CLI.
//!
//! m7e surface: mutating tools (`key`, `launch`) take a REQUIRED explicit
//! `confirm` argument — the MCP equivalent of the CLI's `--confirm` per-op
//! gate. A missing/false/blank confirmation is refused with
//! CONFIRMATION_REQUIRED and audited as a denied actions row exactly like
//! the CLI path (same `authorize_mutation`, same store). `recent_actions`
//! exposes the audit log read-only. Transport wiring (stdio) lands with
//! m7c serve; this module owns the tool handlers and the pure decision
//! seam both the handler and tests drive.
//!
//! Testing note: fastmcp's `McpContext` has no out-of-band constructor
//! (`Cx::for_testing` is `cfg(any(test, feature = "test-internals"))` on
//! the asupersync dependency, invisible to this crate), so tests exercise
//! the extracted `decide()` seam — the exact function `call()` invokes
//! after arg mapping — against a real temp-dir store. `call()` itself adds
//! only arg parsing and result wrapping.
#![forbid(unsafe_code)]

use fastmcp::server::{ToolExecutionMode, ToolHandler};
use fastmcp::{Content, McpContext, McpError, McpErrorCode, McpResult, Tool, ToolAnnotations};
use zestroku_core::mutation::{MutationContext, MutationError, authorize_mutation};
use zestroku_core::store::{ActionRow, SqliteStore, Store};
use zestroku_proto::Command;
use zestroku_types::{AccessTier, Device};

/// Refusal code must match the core gate's action-log value.
pub const CONFIRMATION_REQUIRED: &str = "CONFIRMATION_REQUIRED";

/// Store path contract shared with the CLI (one audit trail). ZESTROKU_DB
/// overrides for tests.
fn store_path() -> std::path::PathBuf {
    std::env::var_os("ZESTROKU_DB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let mut p =
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()));
            p.push(".local/state/zestroku/actions.db");
            p
        })
}

/// Extract the opaque approval token; booleans and arbitrary values never authorize.
pub fn confirmation_token_from(args: &serde_json::Value) -> Option<&str> {
    args.get("confirm").and_then(serde_json::Value::as_str)
}

fn mcp_error(code: McpErrorCode, message: impl Into<String>) -> McpError {
    McpError::new(code, message.into())
}

fn device_for(host: &str, tier: AccessTier) -> Device {
    Device {
        host: host.to_string(),
        port: 8060,
        model_name: String::new(),
        model_number: String::new(),
        software_version: String::new(),
        is_tv: false,
        access_tier: tier,
    }
}

fn build_command(kind: &str, target: &str) -> Result<Command, McpError> {
    match kind {
        "launch" => {
            let app_id: u32 = target.parse().map_err(|_| {
                mcp_error(
                    McpErrorCode::InvalidParams,
                    format!("app id must be a number, got {target:?}"),
                )
            })?;
            Ok(zestroku_proto::launch(app_id))
        }
        "key" => Ok(zestroku_proto::keypress_raw(&target.to_uppercase())),
        other => Err(mcp_error(
            McpErrorCode::InvalidParams,
            format!("unknown mutation kind {other}"),
        )),
    }
}

/// The decision seam: everything `MutationTool::call` does after arg
/// mapping, without a context. Returns the user-facing text on approval or
/// the error (already audited) on refusal. Arg-shaped failures come back
/// as `Err(ArgError)` and are never audited.
#[derive(Debug)]
pub struct ArgError(pub String);

pub fn decide<S: Store + ?Sized>(
    store: &S,
    kind: &str,
    host: &str,
    target: &str,
    tier: AccessTier,
    confirmation: Option<&str>,
    before_state: &str,
) -> Result<Result<String, ArgError>, MutationError> {
    let command = build_command(kind, target).map_err(|e| ArgError(e.to_string()));
    let command = match command {
        Ok(c) => c,
        Err(arg) => return Ok(Err(arg)),
    };
    let context = MutationContext {
        client: "mcp",
        surface: kind,
        intent: &format!("{kind} {target}"),
        before_state,
    };
    match authorize_mutation(
        store,
        &device_for(host, tier),
        &command,
        context,
        confirmation,
    ) {
        Ok(action_id) => Ok(Ok(format!(
            "approved: audit id {action_id}; dispatch happens in serve (m7c)"
        ))),
        Err(e) => Err(e),
    }
}

/// One mutating MCP tool: `kind` is "key" or "launch".
pub struct MutationTool {
    pub kind: &'static str,
}

impl ToolHandler for MutationTool {
    fn definition(&self) -> Tool {
        Tool {
            name: self.kind.to_string(),
            description: Some(format!(
                "{} mutation through the per-op confirmation gate (m7e). \
                 Requires a single-use approval token minted by the owner CLI. \
                 Unconfirmed calls are refused with {CONFIRMATION_REQUIRED} \
                 and logged as denied.",
                self.kind
            )),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "host": {"type": "string", "description": "device host"},
                    "target": {
                        "type": "string",
                        "description": if self.kind == "launch" { "app id" } else { "ECP key name" }
                    },
                    "confirm": {
                        "type": "string",
                        "description": "opaque single-use token from `zestroku approve`"
                    }
                },
                "required": ["host", "target", "confirm"]
            }),
            output_schema: None,
            icon: None,
            version: None,
            tags: vec!["zestroku".to_string()],
            annotations: Some(ToolAnnotations {
                destructive: Some(true),
                ..Default::default()
            }),
        }
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        // Store write only; never the network. m7c may promote to Async
        // when dispatch folds in behind the gate.
        ToolExecutionMode::Blocking
    }

    fn call(&self, _ctx: &McpContext, arguments: serde_json::Value) -> McpResult<Vec<Content>> {
        let host = arguments
            .get("host")
            .and_then(|v| v.as_str())
            .ok_or_else(|| mcp_error(McpErrorCode::InvalidParams, "missing `host`"))?
            .to_string();
        let target = arguments
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or_else(|| mcp_error(McpErrorCode::InvalidParams, "missing `target`"))?
            .to_string();
        let confirmation = confirmation_token_from(&arguments);
        // Tier mirrors the CLI's ZESTROKU_TIER operator override (the live
        // Express is Permissive since the M3 flip); Open stays the fail-safe
        // default until discovery populates tier in m7c.
        let tier = match std::env::var("ZESTROKU_TIER").as_deref() {
            Ok("permissive") | Ok("Permissive") => AccessTier::Permissive,
            _ => AccessTier::Open,
        };
        let store = SqliteStore::open(&store_path()).map_err(|e| {
            mcp_error(
                McpErrorCode::InternalError,
                format!("action log unavailable: {e}"),
            )
        })?;
        match decide(
            &store,
            self.kind,
            &host,
            &target,
            tier,
            confirmation,
            "unavailable (m7c wires the probe)",
        ) {
            Ok(Ok(text)) => Ok(vec![Content::Text { text }]),
            Ok(Err(arg)) => Err(mcp_error(McpErrorCode::InvalidParams, arg.0)),
            Err(MutationError::ConfirmationRequired { action_id, .. }) => Err(mcp_error(
                McpErrorCode::Custom(-32001),
                format!("{CONFIRMATION_REQUIRED}: audit id {action_id}"),
            )),
            Err(MutationError::TierRefused(e)) => Err(mcp_error(
                McpErrorCode::Custom(-32002),
                format!("tier refused: {e} (denial audited)"),
            )),
            Err(MutationError::NotMutation) => Err(mcp_error(
                McpErrorCode::InternalError,
                "internal: non-POST reached the gate",
            )),
            Err(e) => Err(mcp_error(
                McpErrorCode::InternalError,
                format!("mutation gate: {e}"),
            )),
        }
    }
}

/// Read-only audit surface: the N most recent actions rows, newest first.
pub struct RecentActionsTool {
    pub limit: usize,
}

impl ToolHandler for RecentActionsTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "recent_actions".to_string(),
            description: Some(
                "Read-only audit log: the most recent mutation decisions \
                 (approved/denied, result codes, before-state)."
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100}
                }
            }),
            output_schema: None,
            icon: None,
            version: None,
            tags: vec!["zestroku".to_string()],
            annotations: Some(ToolAnnotations {
                read_only: Some(true),
                ..Default::default()
            }),
        }
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::Blocking
    }

    fn call(&self, _ctx: &McpContext, arguments: serde_json::Value) -> McpResult<Vec<Content>> {
        let limit = arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).min(self.limit))
            .unwrap_or(self.limit);
        let store = SqliteStore::open(&store_path()).map_err(|e| {
            mcp_error(
                McpErrorCode::InternalError,
                format!("action log unavailable: {e}"),
            )
        })?;
        let mut rows: Vec<ActionRow> = store
            .list_actions()
            .map_err(|e| mcp_error(McpErrorCode::InternalError, format!("action log: {e}")))?;
        rows.sort_by_key(|r| std::cmp::Reverse(r.id));
        let lines: Vec<String> = rows
            .iter()
            .take(limit)
            .map(|r| {
                format!(
                    "#{} at={} client={} surface={} intent={:?} decision={} result={:?} before={}",
                    r.id, r.at, r.client, r.surface, r.intent, r.decision, r.result, r.before_state
                )
            })
            .collect();
        Ok(vec![Content::Text {
            text: if lines.is_empty() {
                "no actions recorded".to_string()
            } else {
                lines.join("\n")
            },
        }])
    }
}

/// Connectivity check: returns `text` unchanged. Proves the stdio
/// handshake end to end without touching the store or the network.
pub struct EchoTool;

impl ToolHandler for EchoTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "echo".to_string(),
            description: Some("Echo the given text back unchanged (connectivity check).".into()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"]
            }),
            output_schema: None,
            icon: None,
            version: None,
            tags: vec!["zestroku".to_string()],
            annotations: Some(ToolAnnotations {
                read_only: Some(true),
                ..Default::default()
            }),
        }
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::Blocking
    }

    fn call(&self, _ctx: &McpContext, arguments: serde_json::Value) -> McpResult<Vec<Content>> {
        let text = arguments
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| mcp_error(McpErrorCode::InvalidParams, "missing `text`"))?;
        Ok(vec![Content::Text {
            text: text.to_string(),
        }])
    }
}

/// Render stored devices, one line each. Only fields the store already
/// holds privately-safe: id (`host:port`), model number, firmware, tier,
/// last-seen. No serial, UDN, MAC, ad-id or friendly name is ever stored.
pub fn render_devices(rows: &[zestroku_core::store::DeviceRow]) -> String {
    if rows.is_empty() {
        return "no devices recorded (run `zestroku devices`)".to_string();
    }
    rows.iter()
        .map(|d| {
            format!(
                "{} model={} firmware={} tier={} last_seen={}",
                d.id, d.model_num, d.firmware, d.access_tier, d.last_seen
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Read-only inventory: the devices the CLI discovery sweep persisted.
/// Store read only, so it works from a process tree that cannot open LAN
/// sockets.
pub struct ListDevicesTool;

impl ToolHandler for ListDevicesTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "list_devices".to_string(),
            description: Some(
                "Read-only inventory of discovered Roku devices (from the shared store)."
                    .to_string(),
            ),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            output_schema: None,
            icon: None,
            version: None,
            tags: vec!["zestroku".to_string()],
            annotations: Some(ToolAnnotations {
                read_only: Some(true),
                ..Default::default()
            }),
        }
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::Blocking
    }

    fn call(&self, _ctx: &McpContext, _arguments: serde_json::Value) -> McpResult<Vec<Content>> {
        let store = SqliteStore::open(&store_path()).map_err(|e| {
            mcp_error(
                McpErrorCode::InternalError,
                format!("store unavailable: {e}"),
            )
        })?;
        let rows = store
            .list_devices()
            .map_err(|e| mcp_error(McpErrorCode::InternalError, format!("store: {e}")))?;
        Ok(vec![Content::Text {
            text: render_devices(&rows),
        }])
    }
}

/// The MCP server: one tool per implemented CLI surface (inventory, audit
/// log, gated key/launch) plus `echo`.
pub fn server() -> fastmcp::auto::Server {
    fastmcp::auto::server_builder("zestroku", env!("CARGO_PKG_VERSION"))
        .tool(EchoTool)
        .tool(ListDevicesTool)
        .tool(RecentActionsTool { limit: 20 })
        .tool(MutationTool { kind: "key" })
        .tool(MutationTool { kind: "launch" })
        .build()
}

/// Serve the MCP tools over stdio under one asupersync runtime. Returns
/// only on setup failure: a running stdio server ends the process.
pub fn run_stdio() -> Result<(), String> {
    use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
    let reactor = create_reactor().map_err(|e| format!("create I/O reactor: {e}"))?;
    let runtime = RuntimeBuilder::current_thread()
        .with_reactor(reactor)
        .blocking_threads(0, 16)
        .build()
        .map_err(|e| format!("build asupersync runtime: {e}"))?;
    runtime.block_on(async {
        let cx = asupersync::Cx::current().ok_or("runtime installs no ambient Cx")?;
        server().run_stdio_with_cx(&cx).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One exclusive temp store per test; the TempDir stays bound for the
    /// test's scope so the DB file outlives every assertion on it.
    fn store_in_tmp() -> (tempfile::TempDir, SqliteStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.db");
        let store = SqliteStore::open(&path).expect("temp store opens");
        (dir, store)
    }

    #[test]
    fn confirm_mapping_accepts_only_string_tokens() {
        assert_eq!(confirmation_token_from(&json!({"confirm": true})), None);
        assert_eq!(
            confirmation_token_from(&json!({"confirm": "opaque"})),
            Some("opaque")
        );
        assert_eq!(
            confirmation_token_from(&json!({"confirm": "   "})),
            Some("   ")
        );
        assert_eq!(confirmation_token_from(&json!({})), None);
        assert_eq!(confirmation_token_from(&json!({"confirm": 1})), None);
    }

    #[test]
    fn unconfirmed_launch_is_refused_and_audited_denied() {
        let (_dir, store) = store_in_tmp();
        let err = decide(
            &store,
            "launch",
            "192.0.2.10",
            "12",
            AccessTier::Permissive,
            None,
            "{}",
        )
        .unwrap_err();
        let MutationError::ConfirmationRequired { action_id, .. } = &err else {
            panic!("wrong error: {err:?}");
        };
        let rows = store.list_actions().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, *action_id);
        assert_eq!(rows[0].decision, "denied");
        assert_eq!(rows[0].result, CONFIRMATION_REQUIRED);
        assert_eq!(rows[0].client, "mcp");
        assert_eq!(rows[0].surface, "launch");
        assert_eq!(rows[0].intent, "launch 12");
    }

    #[test]
    fn minted_token_approves_exact_launch_once() {
        let (_dir, store) = store_in_tmp();
        let host = "192.0.2.10";
        let device = device_for(host, AccessTier::Permissive);
        let command = zestroku_proto::launch(12);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let token =
            zestroku_core::mutation::issue_approval(&store, &device, &command, now).unwrap();
        let text = decide(
            &store,
            "launch",
            host,
            "12",
            AccessTier::Permissive,
            Some(&token),
            r#"{"app":"home"}"#,
        )
        .expect("gate error impossible here")
        .expect("token authorizes");
        assert!(text.contains("approved: audit id"), "{text}");
        let err = decide(
            &store,
            "launch",
            host,
            "12",
            AccessTier::Permissive,
            Some(&token),
            "{}",
        )
        .expect_err("replay refused");
        assert!(matches!(err, MutationError::ConfirmationRequired { .. }));
        let rows = store.list_actions().unwrap();
        assert_eq!(rows[0].decision, "approved");
        assert_eq!(rows[0].result, "pending");
        assert_eq!(rows[0].before_state, r#"{"app":"home"}"#);
        assert_eq!(
            rows[1].result,
            zestroku_core::mutation::CONFIRMATION_INVALID
        );
    }

    #[test]
    fn non_numeric_launch_id_is_arg_error_not_audited() {
        let (_dir, store) = store_in_tmp();
        let arg = decide(
            &store,
            "launch",
            "192.0.2.10",
            "netflix",
            AccessTier::Permissive,
            None,
            "{}",
        )
        .expect("no gate error")
        .expect_err("non-numeric app id refuses as arg error");
        assert!(arg.0.contains("app id must be a number"));
        assert!(
            store.list_actions().unwrap().is_empty(),
            "arg errors skip the gate"
        );
    }

    #[test]
    fn tier_refusal_is_audited_before_confirmation() {
        let (_dir, store) = store_in_tmp();
        // Open tier refuses mutations even with confirmation — the tier
        // wall precedes the confirmation gate (load-bearing ordering).
        let err = decide(
            &store,
            "key",
            "192.0.2.10",
            "home",
            AccessTier::Open,
            None,
            "{}",
        )
        .expect_err("open tier refuses mutations");
        assert!(matches!(err, MutationError::TierRefused(_)));
        let rows = store.list_actions().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].result, "TIER_REFUSED");
    }
}
