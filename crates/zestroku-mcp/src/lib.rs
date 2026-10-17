//! zestroku-mcp — MCP server tools over the same core gates as the CLI.
//!
//! `list_devices`, `info`, `active_app`, and `apps` read through the configured
//! daemon API using FastMCP's asynchronous request context. They never open
//! direct ECP connections or synthesize local results when the daemon is down.
//! `echo` is local; `recent_actions` reads the shared audit store.
//!
//! `key` and `launch` forward the opaque owner approval token to the
//! configured loopback daemon, which authorizes through the shared core gate
//! and dispatches exactly once. These handlers never consume approvals
//! locally, never open direct ECP connections, and return a tool error
//! (never approval-only success) when the daemon refuses, fails, or is down.
//!
//! `run_stdio` owns the MCP stdio transport and its asupersync runtime. The
//! CLI's real-process integration test exercises it through the actual daemon
//! and loopback simulator.
//!
//! Unit tests exercise the extracted `decide()` local-gate seam; the live
//! mutation path dispatches through the daemon (see `MutationTool`).
//! FastMCP's `McpContext` has no public test constructor because
//! `Cx::for_testing` is gated inside the dependency.
#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use asupersync::http::Client as HttpClient;
use asupersync::{CancelReason, Cx, Outcome};
use fastmcp::server::{BoxFuture, ToolExecutionMode, ToolHandler};
use fastmcp::{Content, McpContext, McpError, McpErrorCode, McpResult, Tool, ToolAnnotations};
use zestroku_core::mutation::{MutationContext, MutationError, authorize_mutation};
use zestroku_core::policy::Policy;
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
        "key" => Ok(zestroku_proto::keypress_raw(target)),
        other => Err(mcp_error(
            McpErrorCode::InvalidParams,
            format!("unknown mutation kind {other}"),
        )),
    }
}

/// The local gate seam exercised by unit tests: arg mapping plus the
/// shared-core authorization, audited locally. The live mutation path does
/// NOT call this: the daemon is the sole authorizer (single-use tokens
/// cannot be consumed twice), so this seam only proves gate semantics
/// offline. Arg-shaped failures come back as `Err(ArgError)` and are never
/// audited.
#[derive(Debug)]
pub struct ArgError(pub String);

/// Inputs to the local mutation gate; one struct keeps the seam under the
/// argument-count lint as policy inputs grow.
pub struct DecideInput<'a> {
    pub kind: &'a str,
    pub host: &'a str,
    pub target: &'a str,
    pub tier: AccessTier,
    pub confirmation: Option<&'a str>,
    pub before_state: &'a str,
    pub policy: &'a Policy,
}

pub fn decide<S: Store + ?Sized>(
    store: &S,
    input: DecideInput<'_>,
) -> Result<Result<String, ArgError>, MutationError> {
    let DecideInput {
        kind,
        host,
        target,
        tier,
        confirmation,
        before_state,
        policy,
    } = input;
    let command = build_command(kind, target).map_err(|e| ArgError(e.to_string()));
    let command = match command {
        Ok(command) => command,
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
        policy,
    ) {
        Ok(action_id) => Ok(Ok(format!(
            "approved: audit id {action_id}; dispatch happens in serve (m7c)"
        ))),
        Err(e) => Err(e),
    }
}

/// One mutating MCP tool: `kind` is "key" or "launch". Mutations dispatch
/// through the configured loopback daemon; the client is shared per server.
pub struct MutationTool {
    pub kind: &'static str,
    client: Arc<HttpClient>,
    base_url: Arc<str>,
}

impl MutationTool {
    fn new(kind: &'static str, client: Arc<HttpClient>, base_url: Arc<str>) -> Self {
        Self {
            kind,
            client,
            base_url,
        }
    }

    /// Map one daemon mutation response: 2xx must carry the daemon's audited
    /// outcome shape (integer action id, string target/surface, outcome
    /// exactly "completed") and is returned verbatim; anything else is a
    /// tool error. The daemon, not this handler, owns authorization and
    /// the audit row, and every device failure arrives as non-2xx.
    fn mutation_content(
        response: asupersync::http::h1::Response,
    ) -> Outcome<Vec<Content>, McpError> {
        let body = match String::from_utf8(response.body) {
            Ok(body) => body,
            Err(_) => {
                return Outcome::Err(mcp_error(
                    McpErrorCode::ToolExecutionError,
                    "daemon_invalid_response: response is not UTF-8",
                ));
            }
        };
        if !(200..300).contains(&response.status) {
            let code = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "daemon_http_error".to_string());
            // The tool contract names confirmation failures
            // CONFIRMATION_REQUIRED (see the tool description); the daemon's
            // refusal is audited daemon-side, never approved here.
            if response.status == 403 && code == "confirmation_required" {
                return Outcome::Err(mcp_error(
                    McpErrorCode::Custom(-32001),
                    format!("{CONFIRMATION_REQUIRED}: daemon refused the token (HTTP 403)"),
                ));
            }
            return Outcome::Err(mcp_error(
                McpErrorCode::ToolExecutionError,
                format!("daemon API error: {code} (HTTP {})", response.status),
            ));
        }
        let value: serde_json::Value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(_) => {
                return Outcome::Err(mcp_error(
                    McpErrorCode::ToolExecutionError,
                    "daemon_invalid_response: mutation outcome is not JSON",
                ));
            }
        };
        // The daemon returns 2xx only on the completed arm, so a 2xx body
        // must also carry outcome "completed"; anything else is a mismatch.
        let shape = value
            .get("action_id")
            .and_then(serde_json::Value::as_i64)
            .is_some()
            && ["target", "surface"].iter().all(|field| {
                value
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .is_some()
            })
            && value.get("outcome").and_then(serde_json::Value::as_str) == Some("completed");
        if !shape {
            return Outcome::Err(mcp_error(
                McpErrorCode::ToolExecutionError,
                "daemon_response_schema_mismatch",
            ));
        }
        Outcome::Ok(vec![Content::Text { text: body }])
    }
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
        ToolExecutionMode::Async
    }

    fn call(&self, _ctx: &McpContext, _arguments: serde_json::Value) -> McpResult<Vec<Content>> {
        Err(mcp_error(
            McpErrorCode::InternalError,
            "mutation tools require the asynchronous request context",
        ))
    }

    fn call_async<'a>(
        &'a self,
        ctx: &'a McpContext,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Outcome<Vec<Content>, McpError>> {
        self.call_async_in_request(ctx, ctx.cx(), arguments)
    }

    fn call_async_in_request<'a>(
        &'a self,
        _ctx: &'a McpContext,
        request_cx: &'a Cx,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Outcome<Vec<Content>, McpError>> {
        Box::pin(async move {
            let Some(host) = arguments.get("host").and_then(|v| v.as_str()) else {
                return Outcome::Err(mcp_error(McpErrorCode::InvalidParams, "missing `host`"));
            };
            let Some(target) = arguments.get("target").and_then(|v| v.as_str()) else {
                return Outcome::Err(mcp_error(McpErrorCode::InvalidParams, "missing `target`"));
            };
            let Some(confirm) = confirmation_token_from(&arguments) else {
                // A missing token is refused before any I/O: there is nothing
                // the daemon could authorize, and no audit row exists for it.
                return Outcome::Err(mcp_error(
                    McpErrorCode::Custom(-32001),
                    format!("{CONFIRMATION_REQUIRED}: missing approval token"),
                ));
            };
            // Arg shape is validated locally without any I/O; the daemon is
            // the sole authorizer, so every well-formed call is forwarded and
            // every refusal is a tool error, never approval-only success.
            let body = match self.kind {
                "launch" => match target.parse::<u32>() {
                    Ok(app_id) => serde_json::json!({"app_id": app_id, "confirm": confirm}),
                    Err(_) => {
                        return Outcome::Err(mcp_error(
                            McpErrorCode::InvalidParams,
                            format!("app id must be a number, got {target:?}"),
                        ));
                    }
                },
                "key" => serde_json::json!({"key": target, "confirm": confirm}),
                other => {
                    return Outcome::Err(mcp_error(
                        McpErrorCode::InvalidParams,
                        format!("unknown mutation kind {other}"),
                    ));
                }
            };
            let path = format!("/v1/devices/{}/{}", encode_path_segment(host), self.kind);
            let url = format!("{}{path}", self.base_url);
            let response = match self
                .client
                .request(
                    request_cx,
                    asupersync::http::h1::types::Method::Post,
                    &url,
                    vec![("content-type".to_string(), "application/json".to_string())],
                    body.to_string().into_bytes(),
                )
                .await
            {
                Ok(response) => response,
                Err(asupersync::http::h1::ClientError::Cancelled) => {
                    return Outcome::Cancelled(request_cx.cancel_reason().unwrap_or_else(|| {
                        CancelReason::user("daemon mutation request cancelled")
                    }));
                }
                Err(_) => {
                    return Outcome::Err(mcp_error(
                        McpErrorCode::ToolExecutionError,
                        "daemon_unavailable",
                    ));
                }
            };
            Self::mutation_content(response)
        })
    }
}

/// Undo the latest restorable mutation: resolves the daemon's undo plan,
/// then relaunches the before-app through the ordinary confirmation gate.
/// The daemon is the sole authorizer; this tool forwards (never mints).
pub struct UndoLastTool {
    client: Arc<HttpClient>,
    base_url: Arc<str>,
}

impl UndoLastTool {
    fn new(client: Arc<HttpClient>, base_url: Arc<str>) -> Self {
        Self { client, base_url }
    }

    /// Validate an undo-plan body: integer action id plus app id/name.
    fn parse_undo_plan(body: &str) -> Result<i64, McpError> {
        let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
            mcp_error(
                McpErrorCode::ToolExecutionError,
                "daemon_invalid_response: undo plan is not JSON",
            )
        })?;
        value
            .get("action_id")
            .and_then(serde_json::Value::as_i64)
            .filter(|_| value.get("app_id").is_some() && value.get("app_name").is_some())
            .ok_or_else(|| {
                mcp_error(
                    McpErrorCode::ToolExecutionError,
                    "daemon_response_schema_mismatch",
                )
            })
    }

    async fn daemon_get(
        &self,
        request_cx: &Cx,
        path: &str,
    ) -> Result<asupersync::http::h1::Response, McpError> {
        let url = format!("{}{path}", self.base_url);
        match self.client.send_get(request_cx, &url).await {
            Ok(response) => Ok(response),
            Err(asupersync::http::h1::ClientError::Cancelled) => Err(mcp_error(
                McpErrorCode::InternalError,
                "daemon undo-plan request cancelled",
            )),
            Err(_) => Err(mcp_error(
                McpErrorCode::ToolExecutionError,
                "daemon_unavailable",
            )),
        }
    }
}

impl ToolHandler for UndoLastTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "undo_last".to_string(),
            description: Some(
                "Relaunch the before-app of the latest restorable mutation \
                 through the per-op confirmation gate (m7e). Resolves the \
                 daemon undo plan first; unconfirmed calls are forwarded so \
                 the daemon logs the denial."
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "host": {"type": "string", "description": "device host"},
                    "confirm": {
                        "type": "string",
                        "description": "opaque single-use token from `zestroku approve`; when absent the daemon logs the denial"
                    }
                },
                "required": ["host"]
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
        ToolExecutionMode::Async
    }

    fn call(&self, _ctx: &McpContext, _arguments: serde_json::Value) -> McpResult<Vec<Content>> {
        Err(mcp_error(
            McpErrorCode::InternalError,
            "undo requires the asynchronous request context",
        ))
    }

    fn call_async<'a>(
        &'a self,
        ctx: &'a McpContext,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Outcome<Vec<Content>, McpError>> {
        self.call_async_in_request(ctx, ctx.cx(), arguments)
    }

    fn call_async_in_request<'a>(
        &'a self,
        _ctx: &'a McpContext,
        request_cx: &'a Cx,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Outcome<Vec<Content>, McpError>> {
        Box::pin(async move {
            let Some(host) = arguments.get("host").and_then(|v| v.as_str()) else {
                return Outcome::Err(mcp_error(McpErrorCode::InvalidParams, "missing `host`"));
            };
            if host.is_empty() {
                return Outcome::Err(mcp_error(
                    McpErrorCode::InvalidParams,
                    "host must not be empty",
                ));
            }
            let plan_path = format!("/v1/devices/{}/undo-plan", encode_path_segment(host));
            let plan = match self.daemon_get(request_cx, &plan_path).await {
                Ok(response) => response,
                Err(error) => return Outcome::Err(error),
            };
            if !(200..300).contains(&plan.status) {
                return Outcome::Err(mcp_error(
                    McpErrorCode::ToolExecutionError,
                    format!("daemon undo-plan error (HTTP {})", plan.status),
                ));
            }
            let plan_text = match String::from_utf8(plan.body) {
                Ok(text) => text,
                Err(_) => {
                    return Outcome::Err(mcp_error(
                        McpErrorCode::ToolExecutionError,
                        "daemon_invalid_response: undo plan is not UTF-8",
                    ));
                }
            };
            let action_id = match Self::parse_undo_plan(&plan_text) {
                Ok(id) => id,
                Err(error) => return Outcome::Err(error),
            };
            // Missing confirm is forwarded (not refused locally) so the
            // daemon writes the denial audit row.
            let mut undo = serde_json::json!({"action_id": action_id});
            if let Some(confirm) = confirmation_token_from(&arguments) {
                undo["confirm"] = serde_json::Value::from(confirm);
            }
            let path = format!("/v1/devices/{}/undo", encode_path_segment(host));
            let url = format!("{}{path}", self.base_url);
            let response =
                match self
                    .client
                    .request(
                        request_cx,
                        asupersync::http::h1::types::Method::Post,
                        &url,
                        vec![("content-type".to_string(), "application/json".to_string())],
                        undo.to_string().into_bytes(),
                    )
                    .await
                {
                    Ok(response) => response,
                    Err(asupersync::http::h1::ClientError::Cancelled) => {
                        return Outcome::Cancelled(request_cx.cancel_reason().unwrap_or_else(
                            || CancelReason::user("daemon undo request cancelled"),
                        ));
                    }
                    Err(_) => {
                        return Outcome::Err(mcp_error(
                            McpErrorCode::ToolExecutionError,
                            "daemon_unavailable",
                        ));
                    }
                };
            MutationTool::mutation_content(response)
        })
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

const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:8070";
const MAX_DAEMON_RESPONSE_BYTES: usize = 1_048_576;

#[derive(Clone, Copy)]
enum ReadOperation {
    Devices,
    Info,
    ActiveApp,
    Apps,
}

impl ReadOperation {
    fn name(self) -> &'static str {
        match self {
            Self::Devices => "list_devices",
            Self::Info => "info",
            Self::ActiveApp => "active_app",
            Self::Apps => "apps",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Devices => "Read-only device inventory from the running zestroku daemon.",
            Self::Info => "Read device information through the daemon; host must match inventory.",
            Self::ActiveApp => "Read the active app through the daemon; host must match inventory.",
            Self::Apps => "Read installed apps through the daemon; host must match inventory.",
        }
    }

    fn input_schema(self) -> serde_json::Value {
        match self {
            Self::Devices => serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            Self::Info | Self::ActiveApp | Self::Apps => serde_json::json!({
                "type": "object",
                "properties": {
                    "host": {
                        "type": "string",
                        "description": "Inventory host or host:port."
                    }
                },
                "required": ["host"],
                "additionalProperties": false
            }),
        }
    }

    fn request_path(self, arguments: &serde_json::Value) -> Result<String, McpError> {
        match self {
            Self::Devices => {
                let Some(object) = arguments.as_object() else {
                    return Err(mcp_error(
                        McpErrorCode::InvalidParams,
                        "list_devices accepts an empty object",
                    ));
                };
                if !object.is_empty() {
                    return Err(mcp_error(
                        McpErrorCode::InvalidParams,
                        "list_devices accepts no arguments",
                    ));
                }
                Ok("/v1/devices".to_string())
            }
            Self::Info | Self::ActiveApp | Self::Apps => {
                let Some(object) = arguments.as_object() else {
                    return Err(mcp_error(
                        McpErrorCode::InvalidParams,
                        "read tool requires a host string",
                    ));
                };
                if object.len() != 1 {
                    return Err(mcp_error(
                        McpErrorCode::InvalidParams,
                        "read tool accepts only the host argument",
                    ));
                }
                let Some(host) = object.get("host").and_then(serde_json::Value::as_str) else {
                    return Err(mcp_error(
                        McpErrorCode::InvalidParams,
                        "read tool requires a host string",
                    ));
                };
                if host.is_empty() {
                    return Err(mcp_error(
                        McpErrorCode::InvalidParams,
                        "host must not be empty",
                    ));
                }
                let target = encode_path_segment(host);
                let suffix = match self {
                    Self::Info => "info",
                    Self::ActiveApp => "active-app",
                    Self::Apps => "apps",
                    Self::Devices => unreachable!(),
                };
                Ok(format!("/v1/devices/{target}/{suffix}"))
            }
        }
    }

    fn accepts_response(self, value: &serde_json::Value) -> bool {
        let exact_fields = |value: &serde_json::Value, fields: &[&str]| {
            value.as_object().is_some_and(|object| {
                object.len() == fields.len()
                    && fields.iter().all(|field| object.contains_key(*field))
            })
        };
        let has_string_fields = |value: &serde_json::Value, fields: &[&str]| {
            fields
                .iter()
                .all(|field| value.get(*field).is_some_and(serde_json::Value::is_string))
        };
        let has_optional_string_fields = |value: &serde_json::Value, fields: &[&str]| {
            fields.iter().all(|field| {
                value
                    .get(*field)
                    .is_some_and(|field| field.is_null() || field.is_string())
            })
        };

        match self {
            Self::Devices => value.as_array().is_some_and(|devices| {
                devices.iter().all(|device| {
                    exact_fields(
                        device,
                        &[
                            "name",
                            "host",
                            "model",
                            "model_number",
                            "software_version",
                            "access_tier",
                            "last_seen",
                        ],
                    ) && has_string_fields(
                        device,
                        &[
                            "name",
                            "host",
                            "model",
                            "model_number",
                            "software_version",
                            "access_tier",
                        ],
                    ) && device
                        .get("last_seen")
                        .and_then(serde_json::Value::as_i64)
                        .is_some()
                })
            }),
            Self::Info => {
                exact_fields(
                    value,
                    &[
                        "host",
                        "model_name",
                        "model_number",
                        "software_version",
                        "is_tv",
                        "access_tier",
                        "tv_active_channel",
                    ],
                ) && has_string_fields(
                    value,
                    &[
                        "host",
                        "model_name",
                        "model_number",
                        "software_version",
                        "access_tier",
                    ],
                ) && value
                    .get("is_tv")
                    .is_some_and(serde_json::Value::is_boolean)
                    && value
                        .get("tv_active_channel")
                        .is_some_and(|channel| channel.is_null() || channel.is_boolean())
            }
            Self::ActiveApp => {
                exact_fields(value, &["host", "app_id", "name"])
                    && has_string_fields(value, &["host", "name"])
                    && value.get("app_id").is_some_and(|app_id| {
                        app_id.is_null()
                            || app_id.as_u64().is_some_and(|id| id <= u64::from(u32::MAX))
                    })
            }
            Self::Apps => {
                exact_fields(value, &["host", "apps"])
                    && has_string_fields(value, &["host"])
                    && value
                        .get("apps")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|apps| {
                            apps.iter().all(|app| {
                                exact_fields(app, &["id", "name", "type", "subtype", "version"])
                                    && has_string_fields(app, &["id", "name", "type"])
                                    && has_optional_string_fields(app, &["subtype", "version"])
                            })
                        })
            }
        }
    }
}

fn encode_path_segment(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        if matches!(
            byte,
            b'a'..=b'z'
                | b'A'..=b'Z'
                | b'0'..=b'9'
                | b'-'
                | b'.'
                | b'_'
                | b'~'
                | b':'
                | b'['
                | b']'
        ) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

fn daemon_base_url() -> Result<String, String> {
    const INVALID_URL: &str = "ZESTROKU_DAEMON_URL must be http://<loopback-ip>:<nonzero-port>";
    let configured = match std::env::var("ZESTROKU_DAEMON_URL") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => DEFAULT_DAEMON_URL.to_string(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("ZESTROKU_DAEMON_URL must be valid UTF-8".to_string());
        }
    };
    let base_url = configured.trim_end_matches('/');
    let authority = base_url.strip_prefix("http://").ok_or(INVALID_URL)?;
    let address = authority
        .parse::<std::net::SocketAddr>()
        .map_err(|_| INVALID_URL)?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(INVALID_URL.to_string());
    }
    Ok(base_url.to_string())
}

struct DaemonReadTool {
    operation: ReadOperation,
    client: Arc<HttpClient>,
    base_url: Arc<str>,
}

impl DaemonReadTool {
    fn new(operation: ReadOperation, client: Arc<HttpClient>, base_url: Arc<str>) -> Self {
        Self {
            operation,
            client,
            base_url,
        }
    }

    fn response_content(
        &self,
        response: asupersync::http::h1::Response,
    ) -> Result<Vec<Content>, McpError> {
        let body = String::from_utf8(response.body).map_err(|_| {
            mcp_error(
                McpErrorCode::ToolExecutionError,
                "daemon_invalid_response: response is not UTF-8",
            )
        })?;
        let value: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
            mcp_error(
                McpErrorCode::ToolExecutionError,
                "daemon_invalid_response: response is not JSON",
            )
        })?;

        if !(200..300).contains(&response.status) {
            let code = value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("daemon_http_error");
            return Err(mcp_error(
                McpErrorCode::ToolExecutionError,
                format!("daemon API error: {code} (HTTP {})", response.status),
            ));
        }
        if !self.operation.accepts_response(&value) {
            return Err(mcp_error(
                McpErrorCode::ToolExecutionError,
                "daemon_response_schema_mismatch",
            ));
        }
        Ok(vec![Content::Text { text: body }])
    }
}

impl ToolHandler for DaemonReadTool {
    fn definition(&self) -> Tool {
        Tool {
            name: self.operation.name().to_string(),
            description: Some(self.operation.description().to_string()),
            input_schema: self.operation.input_schema(),
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
        ToolExecutionMode::Async
    }

    fn call(&self, _ctx: &McpContext, _arguments: serde_json::Value) -> McpResult<Vec<Content>> {
        Err(mcp_error(
            McpErrorCode::InternalError,
            "daemon read tools require the asynchronous request context",
        ))
    }

    fn call_async<'a>(
        &'a self,
        ctx: &'a McpContext,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Outcome<Vec<Content>, McpError>> {
        self.call_async_in_request(ctx, ctx.cx(), arguments)
    }

    fn call_async_in_request<'a>(
        &'a self,
        _ctx: &'a McpContext,
        request_cx: &'a Cx,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Outcome<Vec<Content>, McpError>> {
        Box::pin(async move {
            let path = match self.operation.request_path(&arguments) {
                Ok(path) => path,
                Err(error) => return Outcome::Err(error),
            };
            let url = format!("{}{path}", self.base_url);
            match self.client.send_get(request_cx, &url).await {
                Ok(response) => match self.response_content(response) {
                    Ok(content) => Outcome::Ok(content),
                    Err(error) => Outcome::Err(error),
                },
                Err(asupersync::http::h1::ClientError::Cancelled) => Outcome::Cancelled(
                    request_cx
                        .cancel_reason()
                        .unwrap_or_else(|| CancelReason::user("daemon read request cancelled")),
                ),
                Err(_) => Outcome::Err(mcp_error(
                    McpErrorCode::ToolExecutionError,
                    "daemon_unavailable",
                )),
            }
        })
    }
}

/// The MCP server: daemon-backed read tools, the audit log, gated mutations,
/// and the local `echo` connectivity check.
pub fn server() -> Result<fastmcp::auto::Server, String> {
    let client = Arc::new(
        HttpClient::builder()
            .max_body_size(MAX_DAEMON_RESPONSE_BYTES)
            .request_timeout(Duration::from_secs(2))
            .no_redirects()
            .no_retries()
            .build(),
    );
    let base_url: Arc<str> = Arc::from(daemon_base_url()?);
    Ok(
        fastmcp::auto::server_builder("zestroku", env!("CARGO_PKG_VERSION"))
            .tool(EchoTool)
            .tool(DaemonReadTool::new(
                ReadOperation::Devices,
                Arc::clone(&client),
                Arc::clone(&base_url),
            ))
            .tool(DaemonReadTool::new(
                ReadOperation::Info,
                Arc::clone(&client),
                Arc::clone(&base_url),
            ))
            .tool(DaemonReadTool::new(
                ReadOperation::ActiveApp,
                Arc::clone(&client),
                Arc::clone(&base_url),
            ))
            .tool(DaemonReadTool::new(
                ReadOperation::Apps,
                Arc::clone(&client),
                Arc::clone(&base_url),
            ))
            .tool(RecentActionsTool { limit: 20 })
            .tool(MutationTool::new(
                "key",
                Arc::clone(&client),
                Arc::clone(&base_url),
            ))
            .tool(MutationTool::new(
                "launch",
                Arc::clone(&client),
                Arc::clone(&base_url),
            ))
            .tool(UndoLastTool::new(client, base_url))
            .build(),
    )
}

/// Serve the MCP tools over stdio under one asupersync runtime. Returns
/// only on setup failure: a running stdio server ends the process.
pub fn run_stdio() -> Result<(), String> {
    use asupersync::runtime::{RuntimeBuilder, reactor::create_reactor};
    let server = server()?;
    let reactor = create_reactor().map_err(|e| format!("create I/O reactor: {e}"))?;
    let runtime = RuntimeBuilder::current_thread()
        .with_reactor(reactor)
        .blocking_threads(0, 16)
        .build()
        .map_err(|e| format!("build asupersync runtime: {e}"))?;
    runtime.block_on(async {
        let cx = asupersync::Cx::current().ok_or("runtime installs no ambient Cx")?;
        server.run_stdio_with_cx(&cx).await
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
    fn daemon_read_response_contract_rejects_schema_drift() {
        let devices = json!([{
            "name": "Roku",
            "host": "127.0.0.1",
            "model": "Express",
            "model_number": "SIMX1",
            "software_version": "15.2.4",
            "access_tier": "permissive",
            "last_seen": 1
        }]);
        assert!(ReadOperation::Devices.accepts_response(&devices));
        assert!(ReadOperation::Devices.accepts_response(&json!([])));
        let mut drifted_device = devices.clone();
        drifted_device[0]["unexpected"] = json!(true);
        assert!(!ReadOperation::Devices.accepts_response(&drifted_device));
        let mut drifted_device_type = devices.clone();
        drifted_device_type[0]["model"] = json!(1);
        assert!(!ReadOperation::Devices.accepts_response(&drifted_device_type));
        let mut drifted_timestamp = devices.clone();
        drifted_timestamp[0]["last_seen"] = json!("1");
        assert!(!ReadOperation::Devices.accepts_response(&drifted_timestamp));

        let info = json!({
            "host": "127.0.0.1",
            "model_name": "Sim Express",
            "model_number": "SIMX1",
            "software_version": "15.2.4",
            "is_tv": false,
            "access_tier": "permissive",
            "tv_active_channel": null
        });
        assert!(ReadOperation::Info.accepts_response(&info));
        let mut drifted_info = info.clone();
        drifted_info.as_object_mut().unwrap().remove("access_tier");
        assert!(!ReadOperation::Info.accepts_response(&drifted_info));
        let mut drifted_boolean = info.clone();
        drifted_boolean["is_tv"] = json!("false");
        assert!(!ReadOperation::Info.accepts_response(&drifted_boolean));
        let mut drifted_optional_boolean = info.clone();
        drifted_optional_boolean["tv_active_channel"] = json!("false");
        assert!(!ReadOperation::Info.accepts_response(&drifted_optional_boolean));

        let active_app = json!({
            "host": "127.0.0.1",
            "app_id": null,
            "name": "Roku"
        });
        assert!(ReadOperation::ActiveApp.accepts_response(&active_app));
        let mut drifted_active_app = active_app.clone();
        drifted_active_app["app_id"] = json!("not-a-number");
        assert!(!ReadOperation::ActiveApp.accepts_response(&drifted_active_app));

        let apps = json!({
            "host": "127.0.0.1",
            "apps": [{
                "id": "1001",
                "name": "Streaming One",
                "type": "appl",
                "subtype": null,
                "version": null
            }]
        });
        assert!(ReadOperation::Apps.accepts_response(&apps));
        let mut drifted_apps = apps.clone();
        drifted_apps["apps"][0]["unexpected"] = json!(true);
        assert!(!ReadOperation::Apps.accepts_response(&drifted_apps));
        let mut drifted_app_id = apps.clone();
        drifted_app_id["apps"][0]["id"] = json!(1001);
        assert!(!ReadOperation::Apps.accepts_response(&drifted_app_id));
        let mut drifted_optional_string = apps.clone();
        drifted_optional_string["apps"][0]["subtype"] = json!(true);
        assert!(!ReadOperation::Apps.accepts_response(&drifted_optional_string));
    }

    #[test]
    fn undo_plan_shape_accepts_typed_plan_and_rejects_drift() {
        assert_eq!(
            UndoLastTool::parse_undo_plan(
                r#"{"action_id": 41, "app_id": 12, "app_name": "Netflix"}"#
            )
            .expect("typed plan parses"),
            41
        );
        for bad in [
            "not json",
            r#"{"action_id": "41", "app_id": 12, "app_name": "Netflix"}"#,
            r#"{"app_id": 12, "app_name": "Netflix"}"#,
            r#"{"action_id": 41, "app_name": "Netflix"}"#,
            r#"{"action_id": 41, "app_id": 12}"#,
        ] {
            assert!(
                UndoLastTool::parse_undo_plan(bad).is_err(),
                "drift must refuse: {bad}"
            );
        }
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
            DecideInput {
                kind: "launch",
                host: "192.0.2.10",
                target: "12",
                tier: AccessTier::Permissive,
                confirmation: None,
                before_state: "{}",
                policy: &Policy::default(),
            },
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
            DecideInput {
                kind: "launch",
                host,
                target: "12",
                tier: AccessTier::Permissive,
                confirmation: Some(&token),
                before_state: r#"{"app":"home"}"#,
                policy: &Policy::default(),
            },
        )
        .expect("gate error impossible here")
        .expect("token authorizes");
        assert!(text.contains("approved: audit id"), "{text}");
        let err = decide(
            &store,
            DecideInput {
                kind: "launch",
                host,
                target: "12",
                tier: AccessTier::Permissive,
                confirmation: Some(&token),
                before_state: "{}",
                policy: &Policy::default(),
            },
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
            DecideInput {
                kind: "launch",
                host: "192.0.2.10",
                target: "netflix",
                tier: AccessTier::Permissive,
                confirmation: None,
                before_state: "{}",
                policy: &Policy::default(),
            },
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
            DecideInput {
                kind: "key",
                host: "192.0.2.10",
                target: "home",
                tier: AccessTier::Open,
                confirmation: None,
                before_state: "{}",
                policy: &Policy::default(),
            },
        )
        .expect_err("open tier refuses mutations");
        assert!(matches!(err, MutationError::TierRefused(_)));
        let rows = store.list_actions().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].result, "TIER_REFUSED");
    }
}
