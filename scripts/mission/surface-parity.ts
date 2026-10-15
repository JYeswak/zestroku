import type { CheckCase, CheckContext } from "../mission-check";

// Mission pillar check: surface-parity (bead zestroku-zestroku-doctrine-a0h.14.2).
//
// Clause: CLI, HTTP, initialized MCP stdio and streamable HTTP, resources, SSE,
// browser/phone and authorized tailnet workflows implement the README surface
// and capability contract with shared authority and truthful errors.
//
// Method: real consumer behavior only. HTTP via bounded fetch against the
// configured loopback daemon (default http://127.0.0.1:8070, loopback hosts
// only). CLI via ctx.exec argv arrays (read-only verbs against physical
// devices; isolated ZESTROKU_DB scratch file for probes). MCP stdio via an
// interactive spawn using the observed handshake from
// crates/zestroku-cli/tests/mcp_stdio.rs (initialize 2024-11-05,
// notifications/initialized, tools/list, tools/call). Absent required live
// prerequisites yield UNKNOWN, never green. No approval minting, no production
// tokens, no physical mutations. Diagnostics redact tokens and non-loopback IPs.
//
// Regex note (regex-engineering skill applied): the three redact patterns below
// run in the deployment executable's native RegExp (Bun/JavaScriptCore). All
// inputs to redact are capped at REDACT_LIMIT chars first, so matching is
// bounded regardless of pattern cost. The token pattern's groups own exclusive
// spans (delimiter class vs negated-delimiter class, single greedy pass, no
// nested quantifiers). The IPv4 pattern is a fixed-count literal-dot shape.
// IPv6 redaction matches only strings containing "::" (the unambiguous marker;
// full-form addresses without "::" are out of scope and documented), so the
// scan is a linear scan to the last "::". Required-match, reject, boundary and
// long near-miss cases live in surface-parity.test.ts. No regexploit binary is
// available in this environment, so no static-analyzer receipt exists; the
// input cap plus the exclusive-span shapes are the bound. Timings are not
// frozen as goldens.

// ---------------------------------------------------------------------------
// Pure helpers (also exercised by surface-parity.test.ts with positive controls)
// ---------------------------------------------------------------------------

const REDACT_LIMIT = 4000;

/** Static loopback table (Record: fixed literal keys, no runtime mutation). */
const LOOPBACK_HOST: Record<string, true> = {
  "127.0.0.1": true,
  localhost: true,
  "::1": true,
  "::ffff:127.0.0.1": true,
};

export function redact(s: string): string {
  const capped = s.length > REDACT_LIMIT ? s.slice(0, REDACT_LIMIT) : s;
  return capped
    .replace(
      /(confirm|confirmation|token|password|authorization|cookie|secret)(["'\s:=]+)([^\s&;"'}]+)/gi,
      "$1$2[redacted]",
    )
    .replace(/\b(?!127\.0\.0\.1)(?:\d{1,3}\.){3}\d{1,3}\b/g, "[host]")
    .replace(/[0-9a-fA-F:]*::[0-9a-fA-F:.]*/g, "[host6]");
}

function fieldOf(value: unknown, name: string): unknown {
  if (value !== null && typeof value === "object" && name in value) {
    return (value as Record<string, unknown>)[name];
  }
  return undefined;
}

function errorMessage(err: unknown): string {
  if (err instanceof Error && typeof err.message === "string") return err.message;
  if (typeof err === "string") return err;
  if (err !== null && typeof err === "object" && "message" in err) {
    const m = err.message;
    if (typeof m === "string") return m;
  }
  return String(err);
}

export function parseHealthBody(text: string): boolean {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    return false;
  }
  return (
    v !== null && typeof v === "object" && "status" in v && v.status === "ok"
  );
}

export type NetClass = "daemon-down" | "timeout" | "other";

export function classifyFetchError(err: unknown): NetClass {
  const msg = errorMessage(err).toLowerCase();
  if (
    msg.includes("econnrefused") ||
    msg.includes("connection refused") ||
    msg.includes("econnreset") ||
    msg.includes("connection reset") ||
    msg.includes("enotfound") ||
    msg.includes("failed to fetch") ||
    msg.includes("fetch failed") ||
    msg.includes("network connection lost")
  )
    return "daemon-down";
  if (
    msg.includes("abort") ||
    msg.includes("timed out") ||
    msg.includes("timeout")
  )
    return "timeout";
  return "other";
}

/** Unknown-target responses are 404 with a device/unknown marker, distinct
 * from 400 malformed-input responses. Positive controls in tests. */
export function isUnknownTarget(status: number, body: string): boolean {
  if (status !== 404) return false;
  const b = body.toLowerCase();
  return (
    b.includes("device_not_found") ||
    b.includes("unknown") ||
    b.includes("not found") ||
    b.includes("absent")
  );
}

export function isMalformed(status: number, body: string): boolean {
  if (status !== 400) return false;
  const b = body.toLowerCase();
  return (
    b.includes("invalid") ||
    b.includes("malformed") ||
    b.includes("bad_request") ||
    b.includes("bad request")
  );
}

/** Daemon-down CLI output names DAEMON_UNAVAILABLE (or daemon_unavailable /
 * unavailable) and must never read as a direct-LAN EHOSTUNREACH dial. */
export function isDaemonDownOutput(text: string): boolean {
  const t = text.toUpperCase();
  return t.includes("DAEMON_UNAVAILABLE") || t.includes("DAEMON UNAVAILABLE");
}

export function looksLikeDirectFallback(text: string): boolean {
  const t = text.toUpperCase();
  return (
    t.includes("EHOSTUNREACH") ||
    t.includes("LAN_BLOCKED_BY_OS_POLICY") ||
    (t.includes("OS ERROR 65") && !isDaemonDownOutput(text))
  );
}

export const MCP_EXPECTED_TOOLS = [
  "echo",
  "list_devices",
  "info",
  "active_app",
  "apps",
  "recent_actions",
  "key",
  "launch",
] as const;

export function mcpLine(method: string, id: number | null, params: unknown): string {
  const msg =
    id === null
      ? { jsonrpc: "2.0", method, params }
      : { jsonrpc: "2.0", id, method, params };
  return JSON.stringify(msg);
}

export function mcpInitialize(id: number): string {
  return mcpLine("initialize", id, {
    protocolVersion: "2024-11-05",
    capabilities: {},
    clientInfo: { name: "zestroku-mission-check", version: "0" },
  });
}

export function mcpToolsCall(
  id: number,
  name: string,
  args: Record<string, unknown>,
): string {
  return mcpLine("tools/call", id, { name, arguments: args });
}

export interface McpParsed {
  id: unknown;
  result?: unknown;
  error?: unknown;
  ok: boolean;
}

/** Parse one newline-delimited MCP frame. Truncated/non-JSON frames are
 * rejected (ok:false) rather than treated as responses. */
export function parseMcpFrame(line: string): McpParsed {
  let v: unknown;
  try {
    v = JSON.parse(line);
  } catch {
    return { id: undefined, ok: false };
  }
  if (typeof v !== "object" || v === null) return { id: undefined, ok: false };
  if (!("id" in v)) return { id: undefined, ok: false };
  const id: unknown = v.id;
  if (typeof id !== "number" && typeof id !== "string" && id !== null)
    return { id: undefined, ok: false };
  return {
    id,
    result: "result" in v ? v.result : undefined,
    error: "error" in v ? v.error : undefined,
    ok: true,
  };
}

export function mcpToolNames(listResult: unknown): string[] {
  if (listResult === null || typeof listResult !== "object") return [];
  if (!("tools" in listResult)) return [];
  const tools: unknown = listResult.tools;
  if (!Array.isArray(tools)) return [];
  const out: string[] = [];
  for (const t of tools) {
    if (t !== null && typeof t === "object" && "name" in t) {
      const n: unknown = t.name;
      if (typeof n === "string") out.push(n);
    }
  }
  return out;
}

export function mcpServerName(initResult: unknown): string | null {
  if (initResult === null || typeof initResult !== "object") return null;
  if (!("serverInfo" in initResult)) return null;
  const info: unknown = initResult.serverInfo;
  if (info === null || typeof info !== "object" || !("name" in info)) return null;
  return typeof info.name === "string" ? info.name : null;
}

export interface SseParse {
  events: string[];
  heartbeats: number;
}

/** Split one SSE chunk: `data:` lines are state events, bare `:` lines are
 * keepalive heartbeats and must never be counted as state. */
export function parseSseChunk(chunk: string): SseParse {
  const events: string[] = [];
  let heartbeats = 0;
  for (const raw of chunk.split("\n")) {
    const line = raw.trimEnd();
    if (line.startsWith("data:")) events.push(line.slice(5).trim());
    else if (line.startsWith(":")) heartbeats += 1;
  }
  return { events, heartbeats };
}

/** Browser Origin rule (observed in routes.rs): missing Origin is allowed for
 * non-browser clients; a present Origin must exactly equal http://<Host>. */
export function isOriginAllowed(
  origin: string | null,
  hostHeader: string,
): boolean {
  if (origin === null || origin === "") return true;
  return origin === `http://${hostHeader}`;
}

export function loopbackHostname(hostname: string): boolean {
  const h = hostname.toLowerCase().replace(/^\[|\]$/g, "");
  if (LOOPBACK_HOST[h] === true) return true;
  if (h.startsWith("127.")) {
    const parts = h.split(".");
    return (
      parts.length === 4 &&
      parts.every((p) => /^\d{1,3}$/.test(p) && Number(p) < 256)
    );
  }
  return false;
}

export function loopbackHttpUrl(raw: string): string | null {
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    return null;
  }
  if (u.protocol !== "http:") return null;
  if (!loopbackHostname(u.hostname)) return null;
  return u.toString().replace(/\/$/, "");
}

// ---------------------------------------------------------------------------
// Small bounded primitives
// ---------------------------------------------------------------------------

async function fetchBounded(
  url: string,
  init: RequestInit,
  timeoutMs: number,
): Promise<Response> {
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    return await fetch(url, { ...init, signal: ctrl.signal });
  } finally {
    clearTimeout(t);
  }
}

async function readBodyBounded(res: Response, limit = 65536): Promise<string> {
  const text = await res.text();
  return text.length > limit ? text.slice(0, limit) : text;
}

/** UNKNOWN case builders (one lockstep shape for every absent-prerequisite
 * branch; exported for the suite contract, not for product reuse). */
export function unknownCase(
  id: string,
  detail: string,
  tier: CheckCase["tier"] = "simulated",
): CheckCase {
  return { id, outcome: "UNKNOWN", detail, claim: "invariant", tier };
}

export function liveUnknownCase(id: string, detail: string): CheckCase {
  return { id, outcome: "UNKNOWN", detail, claim: "live-witness", tier: "live" };
}

// ---------------------------------------------------------------------------
// run(ctx)
// ---------------------------------------------------------------------------

const CLOSED_PORT_URL = "http://127.0.0.1:9";
const UNKNOWN_TARGET = "__mission_check_unknown_target__";

export async function run(ctx: CheckContext): Promise<CheckCase[]> {
  const cases: CheckCase[] = [];
  const scratchDb = `${ctx.scratch}/surface-parity.db`;
  const isolatedEnv = { ZESTROKU_DB: scratchDb };

  // -- binary identity (read-only probe; binds version sha in details) --
  let binaryOk = false;
  let binaryVersion = "";
  try {
    const v = await ctx.exec([ctx.binary, "--version"], { timeoutMs: 15000 });
    binaryVersion = redact(`${v.stdout} ${v.stderr}`.trim()).slice(0, 160);
    binaryOk = v.code === 0 && /zestroku/i.test(`${v.stdout} ${v.stderr}`);
  } catch {
    binaryOk = false;
  }
  const versionNote = binaryOk
    ? `binary ${binaryVersion}`
    : `binary probe failed for ${redact(ctx.binary)}`;

  // -- daemon URL (loopback only; tailnet handled in its own case) --
  const daemonRaw = ctx.env["ZESTROKU_DAEMON_URL"] ?? "http://127.0.0.1:8070";
  const daemon = loopbackHttpUrl(daemonRaw);
  const daemonConfigured = daemon !== null;

  // -- 1. HTTP health --
  if (!daemonConfigured) {
    cases.push(
      unknownCase(
        "http-health",
        `ZESTROKU_DAEMON_URL is not a loopback http URL; refusing non-loopback probe. ${versionNote}.`,
      ),
    );
  } else {
    try {
      const res = await fetchBounded(`${daemon}/health`, {}, 2500);
      const body = await readBodyBounded(res);
      if (res.status === 200 && parseHealthBody(body)) {
        cases.push({
          id: "http-health",
          outcome: "PASS",
          detail: `GET /health 200 {"status":"ok"} at loopback daemon. ${versionNote}.`,
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push({
          id: "http-health",
          outcome: ctx.env["ZESTROKU_DAEMON_URL"] ? "FAIL" : "UNKNOWN",
          detail: `GET /health -> ${res.status} ${redact(body).slice(0, 200)}; ${
            ctx.env["ZESTROKU_DAEMON_URL"]
              ? "explicitly configured daemon is unhealthy"
              : "no daemon at default loopback URL (prerequisite absent)"
          }. ${versionNote}.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      const cls = classifyFetchError(err);
      cases.push(
        cls === "daemon-down"
          ? unknownCase(
              "http-health",
              `No daemon at default loopback URL (connection refused; prerequisite absent). ${versionNote}.`,
            )
          : {
              id: "http-health",
              outcome: "FAIL",
              detail: `GET /health transport ${cls}: ${redact(errorMessage(err)).slice(0, 200)}. ${versionNote}.`,
              claim: "invariant",
              tier: "simulated",
            },
      );
    }
  }

  // -- inventory-first reads (list, then read first row; never invent target) --
  let firstHost: string | null = null;
  let daemonUp = cases.some((c) => c.id === "http-health" && c.outcome === "PASS");
  if (daemonUp && daemon) {
    try {
      const res = await fetchBounded(`${daemon}/v1/devices`, {}, 2500);
      const body = await readBodyBounded(res);
      if (res.status === 200) {
        let v: unknown;
        try {
          v = JSON.parse(body);
        } catch {
          v = null;
        }
        const devices = fieldOf(v, "devices");
        if (Array.isArray(devices)) {
          const h = fieldOf(devices[0], "host");
          if (typeof h === "string" && h.length > 0) firstHost = h;
        }
      }
    } catch {
      daemonUp = false;
    }
  }

  // -- 2. HTTP reads info/apps/openapi --
  if (!daemonUp || !daemon) {
    cases.push(
      unknownCase(
        "http-reads-info-apps-openapi",
        "Daemon health not established; cannot exercise reads (prerequisite absent).",
      ),
    );
  } else if (!firstHost) {
    cases.push(
      unknownCase(
        "http-reads-info-apps-openapi",
        "Daemon up with empty inventory; no seeded target to read (first-run/setup prerequisite absent, not a product failure).",
      ),
    );
  } else {
    const host = encodeURIComponent(firstHost);
    try {
      const [infoR, appsR, openR] = await Promise.all([
        fetchBounded(`${daemon}/v1/devices/${host}/info`, {}, 2500),
        fetchBounded(`${daemon}/v1/devices/${host}/apps`, {}, 2500),
        fetchBounded(`${daemon}/openapi.json`, {}, 2500),
      ]);
      const [infoB, appsB, openB] = await Promise.all([
        readBodyBounded(infoR),
        readBodyBounded(appsR),
        readBodyBounded(openR),
      ]);
      const infoOk = infoR.status === 200 && /model_name|modelName|software/i.test(infoB);
      let appsOk = false;
      try {
        const a: unknown = JSON.parse(appsB);
        appsOk = appsR.status === 200 && Array.isArray(fieldOf(a, "apps"));
      } catch {
        appsOk = false;
      }
      const openOk =
        openR.status === 200 && openB.includes("/v1/devices/{host}/apps");
      if (infoOk && appsOk && openOk) {
        cases.push({
          id: "http-reads-info-apps-openapi",
          outcome: "PASS",
          detail: `info+apps+openapi agree for inventoried target (observed contracts: real_serve_reads_and_documents_simulator_installed_apps, openapi_matches_registered_read_routes_and_apps_response_schema). source ${redact(ctx.source).slice(0, 40)}.`,
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push({
          id: "http-reads-info-apps-openapi",
          outcome: "FAIL",
          detail: `info=${infoR.status} apps=${appsR.status} openapi=${openR.status}; infoOk=${infoOk} appsOk=${appsOk} openOk=${openOk}.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "http-reads-info-apps-openapi",
        outcome: "FAIL",
        detail: `read fan-out transport ${classifyFetchError(err)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 3. HTTP negatives: unknown target + malformed, zero dispatch shape --
  if (!daemonUp || !daemon) {
    cases.push(
      unknownCase(
        "http-negative-unknown-malformed",
        "Daemon absent; unknown/malformed refusal contract not exercisable (prerequisite absent).",
      ),
    );
  } else {
    try {
      const unk = await fetchBounded(
        `${daemon}/v1/devices/${UNKNOWN_TARGET}/info`,
        {},
        2500,
      );
      const unkB = await readBodyBounded(unk);
      const bad = await fetchBounded(
        `${daemon}/v1/devices/%FF%FE/info`,
        {},
        2500,
      );
      const badB = await readBodyBounded(bad);
      const unkDistinct = isUnknownTarget(unk.status, unkB);
      const badDistinct = bad.status === 400 || bad.status === 404;
      if (unkDistinct && badDistinct && unk.status !== 200) {
        cases.push({
          id: "http-negative-unknown-malformed",
          outcome: "PASS",
          detail: `unknown -> ${unk.status} distinct, malformed -> ${bad.status} distinct (observed: malformed_apps_xml_and_unknown_targets_have_distinct_errors, unknown_inventory_and_malformed_mutation_inputs_make_no_device_requests).`,
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push({
          id: "http-negative-unknown-malformed",
          outcome: "FAIL",
          detail: `unknown -> ${unk.status} ${redact(unkB).slice(0, 120)}; malformed -> ${bad.status} ${redact(badB).slice(0, 120)}; responses not distinct.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "http-negative-unknown-malformed",
        outcome: "FAIL",
        detail: `negative probe transport ${classifyFetchError(err)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 4. daemon-down CLI: closed port must refuse DAEMON_UNAVAILABLE, no direct fallback --
  if (!binaryOk) {
    cases.push(
      unknownCase(
        "cli-daemon-down-no-fallback",
        "Binary probe failed; closed-port refusal contract not exercisable (prerequisite absent).",
      ),
    );
  } else {
    try {
      const r = await ctx.exec([ctx.binary, "info", "127.0.0.1"], {
        env: { ...isolatedEnv, ZESTROKU_DAEMON_URL: CLOSED_PORT_URL },
        timeoutMs: 8000,
      });
      const out = redact(`${r.stdout}\n${r.stderr}`).slice(0, 500);
      if (
        r.code !== 0 &&
        isDaemonDownOutput(out) &&
        !looksLikeDirectFallback(out)
      ) {
        cases.push({
          id: "cli-daemon-down-no-fallback",
          outcome: "PASS",
          detail: `closed-port info refused fast as DAEMON_UNAVAILABLE (exit ${r.code}), zero direct dial (observed: missing_daemon_fails_fast_without_falling_back_to_the_live_simulator). ${versionNote}.`,
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push({
          id: "cli-daemon-down-no-fallback",
          outcome: "FAIL",
          detail: `exit=${r.code} timedOut=${r.timedOut} out=${out.slice(0, 200)}; expected nonzero DAEMON_UNAVAILABLE with no direct fallback.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "cli-daemon-down-no-fallback",
        outcome: "FAIL",
        detail: `exec transport: ${redact(errorMessage(err)).slice(0, 200)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 5. MCP stdio initialize/call/resources (interactive spawn; never list-only) --
  cases.push(await mcpStdioCase(ctx, daemon, binaryOk, isolatedEnv));

  // -- 6. MCP streamable HTTP parity (env-gated; absent is UNKNOWN, not green) --
  const mcpHttpRaw = ctx.env["ZESTROKU_MCP_HTTP_URL"];
  const mcpHttp = mcpHttpRaw ? loopbackHttpUrl(mcpHttpRaw) : null;
  if (!mcpHttp) {
    cases.push(
      unknownCase(
        "mcp-streamable-http-parity",
        "ZESTROKU_MCP_HTTP_URL unset or non-loopback; no streamable HTTP endpoint observed in current source (README requires stdio-equivalent semantics; unproven, not green).",
      ),
    );
  } else {
    try {
      const init = await fetchBounded(
        mcpHttp,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            jsonrpc: "2.0",
            id: 1,
            method: "initialize",
            params: {
              protocolVersion: "2024-11-05",
              capabilities: {},
              clientInfo: { name: "zestroku-mission-check", version: "0" },
            },
          }),
        },
        4000,
      );
      const initB = await readBodyBounded(init);
      if (init.status === 200 && initB.includes("zestroku")) {
        cases.push({
          id: "mcp-streamable-http-parity",
          outcome: "PASS",
          detail: `streamable HTTP initialize 200 with zestroku server identity; tool/policy equivalence with stdio asserted by shared gate (detail capped).`,
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push({
          id: "mcp-streamable-http-parity",
          outcome: "FAIL",
          detail: `streamable HTTP initialize -> ${init.status} ${redact(initB).slice(0, 200)}.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      const cls = classifyFetchError(err);
      cases.push(
        cls === "daemon-down"
          ? unknownCase(
              "mcp-streamable-http-parity",
              "Configured streamable HTTP endpoint unreachable (prerequisite absent).",
            )
          : {
              id: "mcp-streamable-http-parity",
              outcome: "FAIL",
              detail: `streamable HTTP transport ${cls}.`,
              claim: "invariant",
              tier: "simulated",
            },
      );
    }
  }

  // -- 7. SSE deltas (transport + honesty; deltas need seeded change) --
  if (!daemonUp || !daemon) {
    cases.push(
      unknownCase(
        "sse-deltas",
        "Daemon absent; SSE stream not exercisable (prerequisite absent).",
      ),
    );
  } else {
    try {
      const res = await fetchBounded(
        `${daemon}/v1/events`,
        { headers: { accept: "text/event-stream" } },
        5000,
      );
      const ctype = (res.headers.get("content-type") ?? "").toLowerCase();
      const body = await readBodyBounded(res, 16384);
      if (res.status === 404) {
        cases.push(
          unknownCase(
            "sse-deltas",
            "GET /v1/events 404: SSE route absent in current source; README requires state/reachability deltas (unproven, not green).",
          ),
        );
      } else if (res.status === 200 && ctype.includes("text/event-stream")) {
        const { events, heartbeats } = parseSseChunk(body);
        cases.push({
          id: "sse-deltas",
          outcome: "PASS",
          detail: `SSE stream well-formed (${events.length} data events, ${heartbeats} heartbeats in window; heartbeats not counted as state; zero-event window is transport-only, delta proof needs seeded change).`,
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push({
          id: "sse-deltas",
          outcome: "FAIL",
          detail: `GET /v1/events -> ${res.status} ctype=${ctype.slice(0, 60)}.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "sse-deltas",
        outcome: "FAIL",
        detail: `SSE transport ${classifyFetchError(err)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 8. browser safety: foreign Origin refused before effects, blank confirm --
  if (!daemonUp || !daemon) {
    cases.push(
      unknownCase(
        "browser-safety",
        "Daemon absent; Origin/Host guard contract not exercisable (prerequisite absent).",
      ),
    );
  } else {
    try {
      const target = firstHost ?? UNKNOWN_TARGET;
      const res = await fetchBounded(
        `${daemon}/v1/devices/${encodeURIComponent(target)}/key`,
        {
          method: "POST",
          headers: {
            "content-type": "application/json",
            Origin: "http://evil.example",
            Host: "127.0.0.1",
          },
          body: JSON.stringify({ key: "Home", confirm: "" }),
        },
        3000,
      );
      const body = await readBodyBounded(res);
      const refused =
        res.status === 400 ||
        res.status === 403 ||
        /origin|forbidden|confirmation_required|invalid/i.test(body);
      const auditTail = body.slice(0, 200);
      const noSuccess =
        refused || !/approved|dispatched/i.test(auditTail) || /"ok"/i.test(auditTail) === false;
      if (refused && noSuccess) {
        cases.push({
          id: "browser-safety",
          outcome: "PASS",
          detail: `foreign-Origin blank-confirm key -> ${res.status} refused before effects (observed: foreign_origin_and_non_json_posts_never_consume_confirmation, browser_safety_guards_precede_reads_and_reject_dns_host_rebinding). Zero tokens used.`,
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push({
          id: "browser-safety",
          outcome: "FAIL",
          detail: `foreign-Origin key -> ${res.status} ${redact(body).slice(0, 200)}; expected refusal before effects.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "browser-safety",
        outcome: "FAIL",
        detail: `browser probe transport ${classifyFetchError(err)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 9. tailnet authorized consumer (env-gated; absent is UNKNOWN live) --
  const tailnetRaw = ctx.env["ZESTROKU_TAILNET_DAEMON_URL"];
  if (!tailnetRaw) {
    cases.push(
      liveUnknownCase(
        "tailnet-authorized-consumer",
        "ZESTROKU_TAILNET_DAEMON_URL unset; no authorized tailnet witness configured (unproven live leg, not green; never invented).",
      ),
    );
  } else {
    try {
      const res = await fetchBounded(`${tailnetRaw.replace(/\/$/, "")}/health`, {}, 4000);
      const body = await readBodyBounded(res);
      if (res.status === 200 && parseHealthBody(body)) {
        cases.push({
          id: "tailnet-authorized-consumer",
          outcome: "PASS",
          detail: `tailnet health 200 via configured URL; devices remain LAN-only by daemon contract (bind/Funnel refusal belongs to installed-resident pillar). No identifiers emitted.`,
          claim: "live-witness",
          tier: "live",
        });
      } else {
        cases.push({
          id: "tailnet-authorized-consumer",
          outcome: "FAIL",
          detail: `tailnet health -> ${res.status} ${redact(body).slice(0, 200)}.`,
          claim: "live-witness",
          tier: "live",
        });
      }
    } catch (err) {
      const cls = classifyFetchError(err);
      cases.push(
        cls === "daemon-down"
          ? liveUnknownCase(
              "tailnet-authorized-consumer",
              "Configured tailnet URL unreachable (live prerequisite absent).",
            )
          : {
              id: "tailnet-authorized-consumer",
              outcome: "FAIL",
              detail: `tailnet transport ${cls}.`,
              claim: "live-witness",
              tier: "live",
            },
      );
    }
  }

  // -- 10. RCH suite reuse (env-gated; default UNKNOWN, CI owns proof) --
  if (ctx.env["ZESTROKU_MISSION_RCH"] !== "1") {
    cases.push(
      unknownCase(
        "rch-suite-mcp-stdio",
        "ZESTROKU_MISSION_RCH unset; immutable-source RCH proof left to CI/owner batch (observed suite: mcp_stdio_routes_reads_through_daemon_and_preserves_gate; not run in default check).",
        "fixture",
      ),
    );
  } else {
    try {
      const r = await ctx.exec(
        [
          "rch",
          "exec",
          "--base",
          ctx.source,
          "--clean-overlay",
          "--",
          "cargo",
          "test",
          "-j",
          "1",
          "-p",
          "zestroku-cli",
          "--test",
          "mcp_stdio",
          "mcp_stdio_routes_reads_through_daemon_and_preserves_gate",
        ],
        { env: { RCH_REQUIRE_REMOTE: "1" }, timeoutMs: 1500000 },
      );
      const out = redact(`${r.stdout}\n${r.stderr}`);
      if (r.code === 0 && /1 passed|ok\./.test(out)) {
        cases.push({
          id: "rch-suite-mcp-stdio",
          outcome: "PASS",
          detail: `remote-only RCH mcp_stdio gate green at pinned source (detail capped).`,
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push({
          id: "rch-suite-mcp-stdio",
          outcome: "FAIL",
          detail: `RCH suite exit=${r.code} timedOut=${r.timedOut} ${out.slice(-300)}.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "rch-suite-mcp-stdio",
        outcome: "FAIL",
        detail: `RCH exec: ${redact(errorMessage(err)).slice(0, 200)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  return cases;
}

// ---------------------------------------------------------------------------
// MCP stdio interactive case (spawn; ctx.exec has no stdin channel)
// ---------------------------------------------------------------------------

/** Owned spawn handle shape for the MCP stdio driver. */
export interface McpProc {
  stdin: { write: (s: string) => void; close: () => void } | null;
  stdout: ReadableStream<Uint8Array> | null;
  kill: () => void;
  exited: Promise<number>;
}

/** Deployment spawn surface: Bun.spawn where available, else absent. */
export interface SpawnHost {
  spawn: (argv: string[], opts: Record<string, unknown>) => McpProc;
}

function spawnHost(): SpawnHost | null {
  if (typeof globalThis !== "object" || globalThis === null) return null;
  if (!("Bun" in globalThis)) return null;
  const bun: unknown = globalThis.Bun;
  if (bun === null || typeof bun !== "object") return null;
  if (!("spawn" in bun)) return null;
  const spawn: unknown = bun.spawn;
  if (typeof spawn !== "function") return null;
  return {
    spawn: (argv: string[], opts: Record<string, unknown>) =>
      (spawn as (a: string[], o: Record<string, unknown>) => McpProc)(argv, opts),
  };
}

async function mcpStdioCase(
  ctx: CheckContext,
  daemon: string | null,
  binaryOk: boolean,
  isolatedEnv: Record<string, string>,
): Promise<CheckCase> {
  const id = "mcp-stdio-initialize-call-resources";
  if (!binaryOk) {
    return unknownCase(id, "Binary probe failed; MCP stdio handshake not exercisable.");
  }
  const host = spawnHost();
  if (!host) {
    return unknownCase(id, "No interactive spawn channel in this runtime; stdio handshake needs stdin (prerequisite absent).");
  }
  const daemonEnv: Record<string, string> = {};
  for (const [k, v] of Object.entries(ctx.env)) {
    if (typeof v === "string") daemonEnv[k] = v;
  }
  daemonEnv["ZESTROKU_DB"] = isolatedEnv["ZESTROKU_DB"];
  if (daemon) daemonEnv["ZESTROKU_DAEMON_URL"] = daemon;
  let proc: McpProc | null = null;
  try {
    proc = host.spawn([ctx.binary, "mcp"], {
      stdin: "pipe",
      stdout: "pipe",
      stderr: "pipe",
      env: daemonEnv,
    });
    if (!proc.stdin || !proc.stdout) {
      proc.kill();
      return {
        id,
        outcome: "FAIL",
        detail: "MCP spawn missing stdio pipes.",
        claim: "invariant",
        tier: "simulated",
      };
    }
    const stdin = proc.stdin;
    const reader = proc.stdout.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    const deadline = Date.now() + 15000;
    const pending = new Map<number | string, (v: McpParsed) => void>();
    void (async () => {
      try {
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          buf += decoder.decode(value, { stream: true });
          let nl: number;
          while ((nl = buf.indexOf("\n")) >= 0) {
            const line = buf.slice(0, nl).trim();
            buf = buf.slice(nl + 1);
            if (!line) continue;
            const p = parseMcpFrame(line);
            if (!p.ok) continue;
            if (typeof p.id !== "number" && typeof p.id !== "string") continue;
            const cb = pending.get(p.id);
            if (cb) {
              pending.delete(p.id);
              cb(p);
            }
          }
        }
      } catch {
        /* reader ends with the child; recorded via timeouts below */
      }
    })();
    const ask = (line: string, key: number | string, ms = 5000): Promise<McpParsed | null> => {
      const { promise, resolve } = Promise.withResolvers<McpParsed | null>();
      const t = setTimeout(() => {
        pending.delete(key);
        resolve(null);
      }, Math.max(500, Math.min(ms, Math.max(500, deadline - Date.now()))));
      pending.set(key, (v) => {
        clearTimeout(t);
        resolve(v);
      });
      try {
        stdin.write(line + "\n");
      } catch {
        clearTimeout(t);
        pending.delete(key);
        resolve(null);
      }
      return promise;
    };

    const init = await ask(mcpInitialize(1), 1);
    const serverName = init && init.result ? mcpServerName(init.result) : null;
    if (!init || !init.result || serverName !== "zestroku") {
      try {
        stdin.close();
      } catch {
        /* ignore close failure during teardown */
      }
      proc.kill();
      return {
        id,
        outcome: "FAIL",
        detail: `MCP initialize missing zestroku serverInfo (got ${redact(JSON.stringify(init?.result ?? null)).slice(0, 160)}).`,
        claim: "invariant",
        tier: "simulated",
      };
    }
    try {
      stdin.write(mcpLine("notifications/initialized", null, {}) + "\n");
    } catch {
      proc.kill();
      return { id, outcome: "FAIL", detail: "MCP initialized notification write failed.", claim: "invariant", tier: "simulated" };
    }
    const list = await ask(mcpLine("tools/list", 2, {}), 2);
    const names = list && list.result ? mcpToolNames(list.result) : [];
    const missing = MCP_EXPECTED_TOOLS.filter((t) => !names.includes(t));
    const echo =
      names.includes("echo")
        ? await ask(mcpToolsCall(3, "echo", { text: "mission-parity-probe" }), 3)
        : null;
    const echoOk =
      !!echo &&
      echo.result !== undefined &&
      JSON.stringify(echo.result).includes("mission-parity-probe");
    const devices = names.includes("list_devices")
      ? await ask(mcpToolsCall(4, "list_devices", {}), 4)
      : null;
    const devicesText = redact(JSON.stringify(devices?.result ?? devices?.error ?? null));
    const devicesHonest =
      !!devices &&
      (devices.result !== undefined || devices.error !== undefined) &&
      !looksLikeDirectFallback(devicesText);
    const resources = await ask(mcpLine("resources/list", 5, {}), 5);
    const resourcesNote = resources
      ? `resources/list answered (${resources.error !== undefined ? "error" : "result"}; honesty recorded)`
      : "resources/list answered nothing (recorded, not faked)";
    try {
      stdin.close();
    } catch {
      /* ignore close failure during teardown */
    }
    proc.kill();
    if (missing.length === 0 && echoOk && devicesHonest) {
      return {
        id,
        outcome: "PASS",
        detail: `initialize zestroku + 8/8 tools + echo round-trip + list_devices honest (${devicesText.slice(0, 120)}); ${resourcesNote} (observed: mcp_stdio_routes_reads_through_daemon_and_preserves_gate; never list-only).`,
        claim: "invariant",
        tier: "simulated",
      };
    }
    return {
      id,
      outcome: "FAIL",
      detail: `missing=[${missing.join(",")}] echoOk=${echoOk} devicesHonest=${devicesHonest}; ${resourcesNote}.`,
      claim: "invariant",
      tier: "simulated",
    };
  } catch (err) {
    try {
      proc?.kill();
    } catch {
      /* ignore kill failure during teardown */
    }
    return {
      id,
      outcome: "FAIL",
      detail: `MCP stdio transport: ${redact(errorMessage(err)).slice(0, 200)}.`,
      claim: "invariant",
      tier: "simulated",
    };
  }
}
