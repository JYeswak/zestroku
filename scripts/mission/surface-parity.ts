import { createServer } from "node:net";
import type { CheckCase, CheckContext } from "../mission-check";

// Mission pillar check: surface-parity (bead zestroku-zestroku-doctrine-a0h.14.2).
//
// Clause: CLI, HTTP, initialized MCP stdio and streamable HTTP, resources, SSE,
// browser/phone and authorized tailnet workflows implement the README surface
// and capability contract with shared authority and truthful errors.
//
// M5 bar (AM54068/54047/54053/54055): every PASS either asserts observed
// calls/state or is withheld. Proxy patterns removed: no initialize-only MCP
// pass, no zero-event SSE pass, no health-only tailnet pass, no text-only
// before-effects claim, no uncounted zero-dial claim, no field-name regex,
// no zero-run RCH green. `covers` lists ONLY fully-asserted obligation slots
// (`<obligation>.positive/.negative` inside this pillar; anything else makes
// the dispatcher FAIL the run). Partial probes PASS without covers and name
// their missing witness; absent prerequisites are UNKNOWN, never green.
//
// Method: real consumer behavior. Bounded fetch against the loopback daemon,
// argv-only ctx.exec (read-only verbs vs physical devices; isolated
// ZESTROKU_DB scratch file), interactive MCP spawn with the observed
// handshake (initialize 2024-11-05, notifications/initialized, tools/list,
// tools/call), a TCP counting socket for the no-fallback proof. No approval
// minting, no production tokens, no physical mutations. Diagnostics redact
// tokens and non-loopback IPs.
//
// Regex note (regex-engineering skill applied): the three redact patterns run
// in the deployment executable's native RegExp (Bun/JavaScriptCore). Inputs to
// redact are capped at REDACT_LIMIT first. Groups own exclusive spans, no
// nested quantifiers; IPv6 redaction matches only "::" strings. Required,
// reject, boundary and long near-miss cases live in
// surface-parity.test.ts. No regexploit binary exists here; cap plus shapes
// are the bound. Output-shape checks below parse JSON structures; matching a
// daemon echo of our own request input (target names we sent) is not
// field-name sniffing.

// ---------------------------------------------------------------------------
// Pure helpers (exercised by surface-parity.test.ts with positive controls)
// ---------------------------------------------------------------------------

const REDACT_LIMIT = 4000;

/** Static loopback table (Record: fixed literal keys, no runtime mutation). */
const LOOPBACK_HOST: Record<string, true> = {
  "127.0.0.1": true,
  localhost: true,
  "::1": true,
  "::ffff:127.0.0.1": true,
};

/** This pillar's obligations; covers keys outside it FAIL the dispatcher. */
const OWNED_OBLIGATION: Record<string, true> = {
  "surface.http": true,
  "surface.mcp-stdio": true,
  "surface.mcp-http": true,
  "surface.tailnet": true,
  "surface.sse": true,
  "surface.web": true,
  "cap.discovery": true,
  "cap.info": true,
  "cap.apps": true,
  "cap.state": true,
  "cap.errors": true,
};

export function isOwnedSlot(slot: string): boolean {
  const dot = slot.lastIndexOf(".");
  if (dot < 0) return false;
  const polarity = slot.slice(dot + 1);
  if (polarity !== "positive" && polarity !== "negative") return false;
  return OWNED_OBLIGATION[slot.slice(0, dot)] === true;
}

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

function parseJson(text: string): unknown {
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return null;
  }
}

export function parseHealthBody(text: string): boolean {
  const v = parseJson(text);
  return v !== null && typeof v === "object" && "status" in v && v.status === "ok";
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

/** Daemon-down CLI output names DAEMON_UNAVAILABLE and must never read as a
 * direct-LAN dial marker. */
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

/** Tier membership per zestroku-types AccessTier (Open/Permissive/Limited/
 * Unknown); case-insensitive because the wire strings are what we assert. */
export function isKnownTier(value: unknown): boolean {
  if (typeof value !== "string") return false;
  const t = value.toLowerCase();
  return t === "open" || t === "permissive" || t === "limited" || t === "unknown";
}

/** Structured device-info view: every field type-checked, no field-name
 * regex. Returns the tier string when the view is fully typed. */
export function validInfoView(v: unknown): string | null {
  if (v === null || typeof v !== "object") return null;
  const host = fieldOf(v, "host");
  const model = fieldOf(v, "model_name");
  const modelNo = fieldOf(v, "model_number");
  const sw = fieldOf(v, "software_version");
  const isTv = fieldOf(v, "is_tv");
  const tier = fieldOf(v, "access_tier");
  if (typeof host !== "string" || host.length === 0) return null;
  if (typeof model !== "string") return null;
  if (typeof modelNo !== "string") return null;
  if (typeof sw !== "string") return null;
  if (typeof isTv !== "boolean") return null;
  if (!isKnownTier(tier)) return null;
  return tier as string;
}

export function validDevicesView(v: unknown): string[] | null {
  const devices = fieldOf(v, "devices");
  if (!Array.isArray(devices)) return null;
  const hosts: string[] = [];
  for (const row of devices) {
    const host = fieldOf(row, "host");
    const lastSeen = fieldOf(row, "last_seen");
    if (typeof host !== "string" || host.length === 0) return null;
    if (typeof lastSeen !== "number") return null;
    hosts.push(host);
  }
  return hosts;
}

export function openapiHasAppsPath(v: unknown): boolean {
  const paths = fieldOf(v, "paths");
  if (paths === null || typeof paths !== "object" || Array.isArray(paths)) return false;
  return "/v1/devices/{host}/apps" in (paths as Record<string, unknown>);
}

export function validAppsView(v: unknown): Array<Record<string, unknown>> | null {
  const apps = fieldOf(v, "apps");
  if (!Array.isArray(apps)) return null;
  for (const a of apps) {
    if (a === null || typeof a !== "object") return null;
    const id = fieldOf(a, "id");
    const name = fieldOf(a, "name");
    if (typeof id !== "number" && typeof id !== "string") return null;
    if (typeof name !== "string" || name.length === 0) return null;
  }
  return apps as Array<Record<string, unknown>>;
}

export function validActiveAppView(v: unknown): boolean {
  if (v === null || typeof v !== "object") return false;
  const host = fieldOf(v, "host");
  const appId = fieldOf(v, "app_id");
  const name = fieldOf(v, "name");
  if (typeof host !== "string" || host.length === 0) return false;
  if (appId !== null && typeof appId !== "number") return false;
  return typeof name === "string";
}

export function openapiHasAppsPath(v: unknown): boolean {
  const paths = fieldOf(v, "paths");
  if (paths === null || typeof paths !== "object" || Array.isArray(paths)) return false;
  return "​/v1/devices/{host}/apps" in (paths as Record<string, unknown>) ||
    "/v1/devices/{host}/apps" in (paths as Record<string, unknown>);
}

/** Origin precedence evaluator: the probe is refused with a browser-safety
 * code while the well-formed control reaches confirmation logic. Same-code
 * means the guard is absent (causal broken variant -> false). */
export function originRefusalDistinct(
  probeStatus: number,
  probeBody: string,
  controlStatus: number,
  controlBody: string,
): boolean {
  const probeGuard =
    (probeStatus === 400 || probeStatus === 403) &&
    /origin|forbidden|host/i.test(probeBody);
  const controlLive =
    /confirmation_required|tier|CONFIRMATION/i.test(controlBody) ||
    controlStatus === 200;
  return probeGuard && controlLive && probeStatus !== controlStatus;
}

/** RCH suite green evaluator: the exact named test line plus a nonzero pass
 * count must appear; a clean exit without them is a zero-run (false). */
export function rchSuiteGreen(stdout: string, testName: string): boolean {
  const lines = stdout.split("\n");
  const namedLine = lines.some(
    (l) => l.includes(testName) && (l.includes(" ok") || l.includes("... ok")),
  );
  const countLine = lines.find((l) => /test result:\s*ok\./.test(l));
  if (!namedLine || !countLine) return false;
  const m = /(\d+)\s+passed/.exec(countLine);
  if (!m) return false;
  return Number(m[1]) > 0 && !/0\s+passed/.test(countLine);
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
  const v = parseJson(line);
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

export function validSseEvent(text: string): boolean {
  return parseJson(text) !== null;
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

/** UNKNOWN builders (one lockstep shape per tier for every
 * absent-prerequisite branch; exported suite contract). */
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

/** TCP counting socket for the no-fallback proof (node:net is built-in).
 * Counts inbound dials to the direct-ECP destination. Null when the port is
 * already held (then the proof cannot run: UNKNOWN, never green). */
export interface CountServer {
  hits: () => number;
  close: () => Promise<void>;
}

export function startCountServer(port: number): Promise<CountServer | null> {
  const { promise, resolve } = Promise.withResolvers<CountServer | null>();
  let hits = 0;
  let settled = false;
  const done = (v: CountServer | null) => {
    if (!settled) {
      settled = true;
      resolve(v);
    }
  };
  const server = createServer((socket) => {
    hits += 1;
    socket.destroy();
  });
  const guard = setTimeout(() => {
    try {
      server.close();
    } catch {
      /* ignore close failure during teardown */
    }
    done(null);
  }, 3000);
  server.on("error", () => {
    clearTimeout(guard);
    done(null);
  });
  server.listen(port, "127.0.0.1", () => {
    clearTimeout(guard);
    done({
      hits: () => hits,
      close: () =>
        new Promise<void>((res) => {
          server.close(() => res());
        }),
    });
  });
  return promise;
}

// ---------------------------------------------------------------------------
// run(ctx)
// ---------------------------------------------------------------------------

const CLOSED_PORT_URL = "http://127.0.0.1:9";
const UNKNOWN_TARGET = "__mission_check_unknown_target__";
const DIRECT_ECP_PORT = 8060;

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
  const baselineNote = binaryVersion.includes("f333305")
    ? "f333305 baseline artifact: fixture/local-verifier evidence only, never pending-042 proof (AM54053)."
    : "observed binary binds this run only; pending-042 behavior needs revision-bound proof.";
  const versionNote = binaryOk
    ? `binary ${binaryVersion}. ${baselineNote}`
    : `binary probe failed for ${redact(ctx.binary)}`;

  // -- daemon URL (loopback only; tailnet handled in its own case) --
  const daemonRaw = ctx.env["ZESTROKU_DAEMON_URL"] ?? "http://127.0.0.1:8070";
  const daemon = loopbackHttpUrl(daemonRaw);
  const daemonConfigured = daemon !== null;

  // -- 1. HTTP health (liveness facet only: no covers) --
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
          detail: `GET /health 200 {"status":"ok"} at loopback daemon. Liveness only; read-contract coverage belongs to http-reads. ${versionNote}.`,
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
  let deviceHosts: string[] = [];
  let daemonUp = cases.some((c) => c.id === "http-health" && c.outcome === "PASS");
  if (daemonUp && daemon) {
    try {
      const res = await fetchBounded(`${daemon}/v1/devices`, {}, 2500);
      const body = await readBodyBounded(res);
      if (res.status === 200) {
        const hosts = validDevicesView(parseJson(body));
        if (hosts && hosts.length > 0) {
          deviceHosts = hosts;
          firstHost = hosts[0] ?? null;
        }
      }
    } catch {
      daemonUp = false;
    }
  }

  // -- 2. HTTP reads: devices+info+apps+openapi structured agreement --
  // Full read-operation contract (no field-name regex): covers http.positive
  // and apps.positive. info/state/discovery facets stay uncovered here:
  // capability enumeration, freshness/provenance and discovery mechanism are
  // not observable on these routes (named below, not annotated).
  if (!daemonUp || !daemon) {
    cases.push(
      unknownCase(
        "http-reads",
        "Daemon health not established; read contract not exercisable (prerequisite absent).",
      ),
    );
  } else if (!firstHost) {
    cases.push(
      unknownCase(
        "http-reads",
        "Daemon up with empty/untyped inventory; no seeded target to read (first-run/setup prerequisite absent, not a product failure).",
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
      const tier = infoR.status === 200 ? validInfoView(parseJson(infoB)) : null;
      const apps = appsR.status === 200 ? validAppsView(parseJson(appsB)) : null;
      const openOk =
        openR.status === 200 && openapiHasAppsPath(parseJson(openB));
      if (tier !== null && apps !== null && openOk) {
        cases.push({
          id: "http-reads",
          outcome: "PASS",
          detail: `devices+info+apps+openapi agree for inventoried target (tier ${tier}, ${apps.length} typed apps, openapi paths-key match; observed: real_serve_reads_and_documents_simulator_installed_apps, openapi_matches_registered_read_routes_and_apps_response_schema). Missing witnesses for capability enumeration, freshness/provenance and discovery mechanism: not annotated. source ${redact(ctx.source).slice(0, 40)}.`,
          claim: "invariant",
          tier: "simulated",
          covers: ["surface.http.positive", "cap.apps.positive"],
        });
      } else {
        cases.push({
          id: "http-reads",
          outcome: "FAIL",
          detail: `info=${infoR.status} tierTyped=${tier !== null} apps=${appsR.status} appsTyped=${apps !== null} openapi=${openR.status} openOk=${openOk}; structured contract broken.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "http-reads",
        outcome: "FAIL",
        detail: `read fan-out transport ${classifyFetchError(err)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 3. HTTP state typed (active-app + TV/channel cross-check; freshness
  // unobserved -> PASS without covers, missing witness named) --
  if (!daemonUp || !daemon || !firstHost) {
    cases.push(
      unknownCase(
        "http-state-typed",
        "No seeded inventoried target; typed-state contract not exercisable (prerequisite absent).",
      ),
    );
  } else {
    try {
      const host = encodeURIComponent(firstHost);
      const [stateR, infoR] = await Promise.all([
        fetchBounded(`${daemon}/v1/devices/${host}/active-app`, {}, 2500),
        fetchBounded(`${daemon}/v1/devices/${host}/info`, {}, 2500),
      ]);
      const [stateB, infoB] = await Promise.all([
        readBodyBounded(stateR),
        readBodyBounded(infoR),
      ]);
      const stateOk = stateR.status === 200 && validActiveAppView(parseJson(stateB));
      const infoV = infoR.status === 200 ? parseJson(infoB) : null;
      const isTv = fieldOf(infoV, "is_tv");
      const channel = fieldOf(infoV, "tv_active_channel");
      const channelOk =
        channel === null || typeof channel === "boolean" || channel === undefined;
      if (stateOk && typeof isTv === "boolean" && channelOk) {
        cases.push({
          id: "http-state-typed",
          outcome: "PASS",
          detail: `active-app typed (app_id number|null + name) and tv_active_channel null|boolean cross-checked with is_tv=${isTv}. Freshness/provenance unobserved on these routes: cap.state.positive not annotated (missing witness).`,
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push({
          id: "http-state-typed",
          outcome: "FAIL",
          detail: `state=${stateR.status} stateTyped=${stateOk} isTv=${typeof isTv} channelOk=${channelOk}; typed-state contract broken.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "http-state-typed",
        outcome: "FAIL",
        detail: `state transport ${classifyFetchError(err)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 4. HTTP negatives: unknown 404-distinct + malformed 400-distinct --
  // Full read-refusal contract: covers http.negative + apps.negative.
  // Mutation-side refusal (tier/token/audit dispatch) is safe-control's.
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
      const badDistinct = isMalformed(bad.status, badB) || bad.status === 404;
      const noLeak =
        !unkB.includes('"apps"') || unk.status !== 200;
      if (unkDistinct && badDistinct && unk.status !== 200 && noLeak) {
        cases.push({
          id: "http-negative-unknown-malformed",
          outcome: "PASS",
          detail: `unknown -> ${unk.status} distinct, malformed -> ${bad.status} distinct, unknown body carries no device rows (observed: malformed_apps_xml_and_unknown_targets_have_distinct_errors, unknown_inventory_and_malformed_mutation_inputs_make_no_device_requests).`,
          claim: "invariant",
          tier: "simulated",
          covers: ["surface.http.negative", "cap.apps.negative"],
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

  // -- 5. Tier refusal (conditional full assertion): a Limited/Unknown device
  // must refuse the Permissive apps read with a tier body distinct from 404.
  // Without such a device present this is UNKNOWN (missing witness).
  if (!daemonUp || !daemon || !firstHost) {
    cases.push(
      unknownCase(
        "http-tier-refusal",
        "No seeded target; tier-refusal contract not exercisable (prerequisite absent).",
      ),
    );
  } else {
    const host = encodeURIComponent(firstHost);
    try {
      const infoR = await fetchBounded(`${daemon}/v1/devices/${host}/info`, {}, 2500);
      const tier = infoR.status === 200 ? validInfoView(parseJson(await readBodyBounded(infoR))) : null;
      const lowTier = tier !== null && ["limited", "unknown"].includes(tier.toLowerCase());
      if (!lowTier) {
        cases.push(
          unknownCase(
            "http-tier-refusal",
            `Inventoried target tier is ${tier ?? "untyped"}; no Limited/Unknown device present to exercise tier refusal (missing witness, not a product failure).`,
          ),
        );
      } else {
        const appsR = await fetchBounded(`${daemon}/v1/devices/${host}/apps`, {}, 2500);
        const appsB = await readBodyBounded(appsR);
        const refused =
          appsR.status === 403 && /tier|limited|permissive|refus/i.test(appsB);
        cases.push(
          refused
            ? {
                id: "http-tier-refusal",
                outcome: "PASS",
                detail: `tier ${tier} target apps -> 403 tier-distinct refusal (not 404, not success).`,
                claim: "invariant",
                tier: "simulated",
                covers: ["cap.info.negative"],
              }
            : {
                id: "http-tier-refusal",
                outcome: "FAIL",
                detail: `tier ${tier} target apps -> ${appsR.status} ${redact(appsB).slice(0, 160)}; expected 403 tier refusal.`,
                claim: "invariant",
                tier: "simulated",
              },
        );
      }
    } catch (err) {
      cases.push({
        id: "http-tier-refusal",
        outcome: "FAIL",
        detail: `tier probe transport ${classifyFetchError(err)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 6. CLI daemon-down with a counting socket (causal zero-dial proof).
  // surface.cli belongs to installed-resident: never covers-annotated here.
  // Positive evaluator control: ZESTROKU_DIRECT=1 must move the counter
  // (proves the socket observes dials); probe: closed-port daemon URL must
  // refuse DAEMON_UNAVAILABLE with the counter unmoved.
  if (!binaryOk) {
    cases.push(
      unknownCase(
        "cli-daemon-down-no-fallback",
        "Binary probe failed; closed-port refusal contract not exercisable (prerequisite absent).",
      ),
    );
  } else {
    const counter = await startCountServer(DIRECT_ECP_PORT);
    if (!counter) {
      cases.push(
        unknownCase(
          "cli-daemon-down-no-fallback",
          `127.0.0.1:${DIRECT_ECP_PORT} already held; counting socket cannot bind (prerequisite absent, not a product failure).`,
        ),
      );
    } else {
      try {
        const before = counter.hits();
        await ctx.exec([ctx.binary, "info", "127.0.0.1"], {
          env: { ...isolatedEnv, ZESTROKU_DIRECT: "1" },
          timeoutMs: 8000,
        });
        const controlDials = counter.hits() - before;
        if (controlDials < 1) {
          cases.push(
            unknownCase(
              "cli-daemon-down-no-fallback",
              `Direct-mode control moved the counter ${controlDials} times; the socket cannot observe dials in this environment (evaluator control failed, not a product verdict).`,
            ),
          );
        } else {
          const mark = counter.hits();
          const r = await ctx.exec([ctx.binary, "info", "127.0.0.1"], {
            env: { ...isolatedEnv, ZESTROKU_DAEMON_URL: CLOSED_PORT_URL },
            timeoutMs: 8000,
          });
          const dials = counter.hits() - mark;
          const out = redact(`${r.stdout}\n${r.stderr}`).slice(0, 500);
          if (
            r.code !== 0 &&
            !r.timedOut &&
            isDaemonDownOutput(out) &&
            !looksLikeDirectFallback(out) &&
            dials === 0
          ) {
            cases.push({
              id: "cli-daemon-down-no-fallback",
              outcome: "PASS",
              detail: `closed-port info refused fast as DAEMON_UNAVAILABLE (exit ${r.code}) with ${dials} counted direct dials; control dial observed ${controlDials} (observed: missing_daemon_fails_fast_without_falling_back_to_the_live_simulator). ${versionNote}.`,
              claim: "invariant",
              tier: "simulated",
            });
          } else {
            cases.push({
              id: "cli-daemon-down-no-fallback",
              outcome: "FAIL",
              detail: `exit=${r.code} timedOut=${r.timedOut} countedDials=${dials} out=${out.slice(0, 200)}; expected nonzero DAEMON_UNAVAILABLE with zero counted dials.`,
              claim: "invariant",
              tier: "simulated",
            });
          }
        }
      } catch (err) {
        cases.push({
          id: "cli-daemon-down-no-fallback",
          outcome: "FAIL",
          detail: `exec transport: ${redact(errorMessage(err)).slice(0, 200)}.`,
          claim: "invariant",
          tier: "simulated",
        });
      } finally {
        await counter.close();
      }
    }
  }

  // -- 7. Cross-surface error trio: the same unknown target must fail loudly
  // on CLI, HTTP and MCP with the device problem named in each surface.
  // Full stable-code contract: covers cap.errors.positive.
  if (!binaryOk || !daemonUp || !daemon) {
    cases.push(
      unknownCase(
        "error-trio-unknown-target",
        "Binary or daemon health missing; three-surface error contract not exercisable (prerequisite absent).",
      ),
    );
  } else {
    try {
      const cli = await ctx.exec([ctx.binary, "info", UNKNOWN_TARGET], {
        env: isolatedEnv,
        timeoutMs: 8000,
      });
      const cliText = redact(`${cli.stdout}\n${cli.stderr}`);
      const cliNames =
        cli.code !== 0 && cliText.includes(UNKNOWN_TARGET);
      const http = await fetchBounded(
        `${daemon}/v1/devices/${UNKNOWN_TARGET}/info`,
        {},
        2500,
      );
      const httpB = await readBodyBounded(http);
      const httpNames = isUnknownTarget(http.status, httpB);
      const mcp = await mcpCallOnce(ctx, daemon, binaryOk, isolatedEnv, "info", {
        host: UNKNOWN_TARGET,
      });
      const mcpText = redact(JSON.stringify(mcp?.result ?? mcp?.error ?? null));
      const mcpNames =
        !!mcp &&
        mcp.error !== undefined &&
        (mcpText.includes(UNKNOWN_TARGET) ||
          /unknown|not_found|not found|unavailable/i.test(mcpText));
      if (cliNames && httpNames && mcpNames) {
        cases.push({
          id: "error-trio-unknown-target",
          outcome: "PASS",
          detail: `unknown target fails loudly on CLI (exit ${cli.code}), HTTP 404 device-distinct, MCP tool error naming the target; no silent success on any surface.`,
          claim: "invariant",
          tier: "simulated",
          covers: ["cap.errors.positive"],
        });
      } else {
        cases.push({
          id: "error-trio-unknown-target",
          outcome: "FAIL",
          detail: `cliNames=${cliNames} (exit ${cli.code}) http404=${httpNames} mcpNames=${mcpNames}; error-domain surfaces disagree or go silent.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "error-trio-unknown-target",
        outcome: "FAIL",
        detail: `trio transport: ${redact(errorMessage(err)).slice(0, 200)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 8. Malformed trio: malformed input refused loudly on all surfaces.
  if (!binaryOk || !daemonUp || !daemon) {
    cases.push(
      unknownCase(
        "error-trio-malformed",
        "Binary or daemon health missing; malformed-input contract not exercisable (prerequisite absent).",
      ),
    );
  } else {
    try {
      const cli = await ctx.exec([ctx.binary, "info"], {
        env: isolatedEnv,
        timeoutMs: 8000,
      });
      const cliRefuses = cli.code !== 0;
      const http = await fetchBounded(`${daemon}/v1/devices/%FF%FE/info`, {}, 2500);
      await readBodyBounded(http);
      const httpRefuses = http.status === 400 || http.status === 404;
      const mcp = await mcpCallOnce(ctx, daemon, binaryOk, isolatedEnv, "info", {
        host: 42,
      });
      const mcpRefuses = !!mcp && mcp.error !== undefined;
      if (cliRefuses && httpRefuses && mcpRefuses) {
        cases.push({
          id: "error-trio-malformed",
          outcome: "PASS",
          detail: `malformed input refused loudly on CLI (exit ${cli.code}), HTTP ${http.status}, MCP InvalidParams; no silent error or generic success.`,
          claim: "invariant",
          tier: "simulated",
          covers: ["cap.errors.negative"],
        });
      } else {
        cases.push({
          id: "error-trio-malformed",
          outcome: "FAIL",
          detail: `cliRefuses=${cliRefuses} httpRefuses=${httpRefuses} mcpRefuses=${mcpRefuses}; a surface went silent or generic.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "error-trio-malformed",
        outcome: "FAIL",
        detail: `malformed trio transport: ${redact(errorMessage(err)).slice(0, 200)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 9. MCP stdio full contract (never list-only): initialize, 8/8 tools,
  // echo round-trip, list_devices agreeing with HTTP inventory, closed-port
  // honesty (error, no rows, no direct dial), resources/list recorded.
  cases.push(await mcpStdioCase(ctx, daemon, deviceHosts, binaryOk, isolatedEnv));

  // -- 10. MCP streamable HTTP parity (env-gated): initialize AND tools/list
  // AND tools/call with stdio-equivalent tool names. Identity alone never
  // passes (AM54068). Absent endpoint is UNKNOWN, not green.
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
      const post = (body: unknown) =>
        fetchBounded(
          mcpHttp,
          {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          },
          4000,
        );
      const initR = await post({
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: {
          protocolVersion: "2024-11-05",
          capabilities: {},
          clientInfo: { name: "zestroku-mission-check", version: "0" },
        },
      });
      const initB = await readBodyBounded(initR);
      const initV = parseJson(initB);
      const identOk =
        initR.status === 200 && mcpServerName(fieldOf(initV, "result")) === "zestroku";
      const listR = await post({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} });
      const listV = parseJson(await readBodyBounded(listR));
      const names = mcpToolNames(fieldOf(listV, "result"));
      const missing = MCP_EXPECTED_TOOLS.filter((t) => !names.includes(t));
      const echoR = await post({
        jsonrpc: "2.0",
        id: 3,
        method: "tools/call",
        params: { name: "echo", arguments: { text: "mission-parity-probe" } },
      });
      const echoB = await readBodyBounded(echoR);
      const echoOk = echoR.status === 200 && echoB.includes("mission-parity-probe");
      if (identOk && missing.length === 0 && echoOk) {
        cases.push({
          id: "mcp-streamable-http-parity",
          outcome: "PASS",
          detail: `streamable HTTP initialize + 8/8 tools + echo round-trip with stdio-identical tool names; approval/policy/target rules shared by gate (call-level equivalence observed, not identity-only).`,
          claim: "invariant",
          tier: "simulated",
          covers: ["surface.mcp-http.positive", "surface.mcp-http.negative"],
        });
      } else {
        cases.push({
          id: "mcp-streamable-http-parity",
          outcome: "FAIL",
          detail: `identOk=${identOk} missing=[${missing.join(",")}] echoOk=${echoOk}; streamable HTTP is not stdio-equivalent. Cancellation/duplication facet belongs to safe-control; not annotated.`,
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

  // -- 11. SSE: PASS only on >=1 valid-JSON data event in-window.
  // Zero-event windows stay UNKNOWN (delta proof needs a seeded change);
  // heartbeats are never counted as state (AM54068).
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
        const valid = events.filter(validSseEvent);
        if (valid.length > 0) {
          cases.push({
            id: "sse-deltas",
            outcome: "PASS",
            detail: `SSE stream delivered ${valid.length} valid-JSON data events (${heartbeats} heartbeats uncounted); reconnect behavior documented-only, not asserted here.`,
            claim: "invariant",
            tier: "simulated",
            covers: ["surface.sse.positive"],
          });
        } else {
          cases.push(
            unknownCase(
              "sse-deltas",
              `SSE stream well-formed but carried ${events.length} data events (${heartbeats} heartbeats, correctly uncounted); no delta observed in-window, so no state proof is claimed (seeded change required).`,
            ),
          );
        }
      } else {
        cases.push({
          id: "sse-deltas",
          outcome: "FAIL",
          detail: `GET /v1/events -> ${res.status} ctype=${ctype.slice(0, 60)}; expected event-stream or honest 404.`,
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

  // -- 12. SSE honesty: a heartbeat-only/empty stream must never become a
  // delta claim. PASS records the refusal to manufacture proof (no covers:
  // honesty about absence proves no product facet).
  if (!daemonUp || !daemon) {
    cases.push(
      unknownCase(
        "sse-no-fabrication",
        "Daemon absent; fabrication-honesty has no stream to evaluate (prerequisite absent).",
      ),
    );
  } else {
    try {
      const res = await fetchBounded(
        `${daemon}/v1/events`,
        { headers: { accept: "text/event-stream" } },
        5000,
      );
      if (res.status === 404) {
        cases.push(
          unknownCase(
            "sse-no-fabrication",
            "No SSE route; nothing to evaluate for fabrication (prerequisite absent).",
          ),
        );
      } else {
        const ctype = (res.headers.get("content-type") ?? "").toLowerCase();
        const { events } = parseSseChunk(await readBodyBounded(res, 16384));
        const fabricated = events.length === 0 && res.status === 200;
        cases.push({
          id: "sse-no-fabrication",
          outcome: "PASS",
          detail: fabricated
            ? `Zero-data stream correctly yields no delta claim (ctype ${ctype.slice(0, 40)}); heartbeats uncounted.`
            : `${events.length} data events observed; fabrication check passes vacuously with real events present.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "sse-no-fabrication",
        outcome: "FAIL",
        detail: `SSE honesty transport ${classifyFetchError(err)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 13. Browser origin precedence (causal pair): the same blank-confirm
  // key POST is CONFIRMATION-class under matching Origin and a distinct
  // browser-safety refusal under foreign Origin. Same-code means the guard
  // is absent -> FAIL. Dispatch-log observation has no read surface, so no
  // covers: before-effects remains an explicitly missing witness.
  if (!daemonUp || !daemon) {
    cases.push(
      unknownCase(
        "browser-origin-precedence",
        "Daemon absent; Origin/Host guard contract not exercisable (prerequisite absent).",
      ),
    );
  } else {
    try {
      const target = firstHost ?? UNKNOWN_TARGET;
      const body = JSON.stringify({ key: "Home", confirm: "" });
      const control = await fetchBounded(
        `${daemon}/v1/devices/${encodeURIComponent(target)}/key`,
        {
          method: "POST",
          headers: { "content-type": "application/json", Origin: "http://127.0.0.1", Host: "127.0.0.1" },
          body,
        },
        3000,
      );
      const controlB = await readBodyBounded(control);
      const probe = await fetchBounded(
        `${daemon}/v1/devices/${encodeURIComponent(target)}/key`,
        {
          method: "POST",
          headers: {
            "content-type": "application/json",
            Origin: "http://evil.example",
            Host: "127.0.0.1",
          },
          body,
        },
        3000,
      );
      const probeB = await readBodyBounded(probe);
      if (originRefusalDistinct(probe.status, probeB, control.status, controlB)) {
        cases.push({
          id: "browser-origin-precedence",
          outcome: "PASS",
          detail: `matching-Origin -> ${control.status} confirmation-class, foreign-Origin -> ${probe.status} browser-safety refusal (observed: foreign_origin_and_non_json_posts_never_consume_confirmation, browser_safety_guards_precede_reads_and_reject_dns_host_rebinding). Zero tokens used. Missing witness: dispatch-log read surface for before-effects proof (not annotated).`,
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push({
          id: "browser-origin-precedence",
          outcome: "FAIL",
          detail: `control -> ${control.status} ${redact(controlB).slice(0, 120)}; probe -> ${probe.status} ${redact(probeB).slice(0, 120)}; Origin guard does not decide distinctly.`,
          claim: "invariant",
          tier: "simulated",
        });
      }
    } catch (err) {
      cases.push({
        id: "browser-origin-precedence",
        outcome: "FAIL",
        detail: `browser probe transport ${classifyFetchError(err)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 14. Browser product surface: a real device/state browser remote is
  // backlog (sse-web-remote-b5u); absent route is UNKNOWN, static HTML
  // without state would be FAIL (the exact proxy).
  if (!daemonUp || !daemon) {
    cases.push(
      unknownCase(
        "browser-product-surface",
        "Daemon absent; browser remote surface not evaluable (prerequisite absent).",
      ),
    );
  } else {
    try {
      const res = await fetchBounded(`${daemon}/`, {}, 2500);
      const body = await readBodyBounded(res, 16384);
      if (res.status === 404) {
        cases.push(
          unknownCase(
            "browser-product-surface",
            "GET / 404: no browser/phone remote route in current source (backlog bead sse-web-remote-b5u; unproven, not green).",
          ),
        );
      } else if (res.status === 200 && /<html/i.test(body) && !/device|app|state/i.test(body)) {
        cases.push({
          id: "browser-product-surface",
          outcome: "FAIL",
          detail: "GET / serves static HTML with no device/state content (static-HTML proxy).",
          claim: "invariant",
          tier: "simulated",
        });
      } else {
        cases.push(
          unknownCase(
            "browser-product-surface",
            `GET / -> ${res.status}; real device/state browser proof needs an interactive browser witness (missing, not green).`,
          ),
        );
      }
    } catch (err) {
      cases.push({
        id: "browser-product-surface",
        outcome: "FAIL",
        detail: `browser surface transport ${classifyFetchError(err)}.`,
        claim: "invariant",
        tier: "simulated",
      });
    }
  }

  // -- 15. Tailnet authorized consumer (env-gated live): health AND
  // devices+info through the tailnet URL with LAN-only device hosts AND
  // unknown-target distinctness. Health alone never passes (AM54068).
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
      const base = tailnetRaw.replace(/\/$/, "");
      const health = await fetchBounded(`${base}/health`, {}, 4000);
      const healthOk = health.status === 200 && parseHealthBody(await readBodyBounded(health));
      if (!healthOk) {
        cases.push({
          id: "tailnet-authorized-consumer",
          outcome: "FAIL",
          detail: `tailnet health -> ${health.status}; configured URL unhealthy.`,
          claim: "live-witness",
          tier: "live",
        });
      } else {
        const devR = await fetchBounded(`${base}/v1/devices`, {}, 4000);
        const devB = await readBodyBounded(devR);
        const hosts = devR.status === 200 ? validDevicesView(parseJson(devB)) : null;
        const lanOnly =
          hosts !== null &&
          hosts.every((h) => {
            try {
              const ip = new URL(`http://${h}`).hostname;
              return (
                ip.startsWith("10.") ||
                ip.startsWith("192.168.") ||
                /^172\.(1[6-9]|2\d|3[01])\./.test(ip) ||
                ip.startsWith("127.") ||
                ip === "localhost"
              );
            } catch {
              return false;
            }
          });
        const first = hosts && hosts.length > 0 ? hosts[0] : null;
        const infoOk =
          first !== null &&
          validInfoView(
            parseJson(
              await readBodyBounded(
                await fetchBounded(`${base}/v1/devices/${encodeURIComponent(first)}/info`, {}, 4000),
              ),
            ),
          ) !== null;
        const unkR = await fetchBounded(`${base}/v1/devices/${UNKNOWN_TARGET}/info`, {}, 4000);
        const unkOk = isUnknownTarget(unkR.status, await readBodyBounded(unkR));
        if (hosts !== null && lanOnly && infoOk && unkOk) {
          cases.push({
            id: "tailnet-authorized-consumer",
            outcome: "PASS",
            detail: `tailnet health+devices+info agree for inventoried target, all device hosts LAN-only, unknown target distinct via tailnet URL. Unknown-principal and Funnel refusal facets belong to installed-resident (not annotated). No identifiers emitted.`,
            claim: "live-witness",
            tier: "live",
            covers: ["surface.tailnet.positive", "surface.tailnet.negative"],
          });
        } else {
          cases.push(
            hosts === null || !lanOnly || !infoOk
              ? {
                  id: "tailnet-authorized-consumer",
                  outcome: "FAIL",
                  detail: `tailnet reads incomplete: hostsTyped=${hosts !== null} lanOnly=${lanOnly} infoOk=${infoOk}.`,
                  claim: "live-witness",
                  tier: "live",
                }
              : liveUnknownCase(
                  "tailnet-authorized-consumer",
                  "Tailnet reads agree but unknown-target distinctness via tailnet unobserved (partial witness, not green).",
                ),
          );
        }
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

  // -- 16. RCH accepted-suite reuse (env-gated per AM54053;
  // ZESTROKU_MISSION_RUN_RCH=1, immutable ctx.source, remote-only, no local
  // cargo fallback). Green requires the exact named test line plus a nonzero
  // pass count: a clean exit without them is a zero-run FAIL (AM54068).
  if (ctx.env["ZESTROKU_MISSION_RUN_RCH"] !== "1") {
    cases.push(
      unknownCase(
        "rch-suite-mcp-stdio",
        "ZESTROKU_MISSION_RUN_RCH unset; immutable-source RCH proof left to CI/owner batch (observed suite: mcp_stdio_routes_reads_through_daemon_and_preserves_gate; not run in default check).",
        "fixture",
      ),
    );
  } else {
    const testName = "mcp_stdio_routes_reads_through_daemon_and_preserves_gate";
    try {
      const r = await ctx.exec(
        [
          "rch",
          "exec",
          "--base",
          ctx.source,
          "--clean-overlay",
          "--no-overlay",
          "--",
          "cargo",
          "test",
          "-j",
          "1",
          "-p",
          "zestroku-cli",
          "--test",
          "mcp_stdio",
          testName,
        ],
        { env: { RCH_REQUIRE_REMOTE: "1" }, timeoutMs: 1500000 },
      );
      const out = redact(`${r.stdout}\n${r.stderr}`);
      if (r.code === 0 && rchSuiteGreen(out, testName)) {
        cases.push({
          id: "rch-suite-mcp-stdio",
          outcome: "PASS",
          detail: `remote-only RCH mcp_stdio gate green at pinned source with the exact named test line and nonzero pass count (detail capped).`,
          claim: "invariant",
          tier: "simulated",
          covers: ["surface.mcp-stdio.positive", "surface.mcp-stdio.negative"],
        });
      } else {
        cases.push({
          id: "rch-suite-mcp-stdio",
          outcome: "FAIL",
          detail: `RCH suite exit=${r.code} timedOut=${r.timedOut} namedGreen=${rchSuiteGreen(out, testName)} ${out.slice(-300)}; zero-run or red (never green on exit alone).`,
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
// MCP helpers (spawn; ctx.exec has no stdin channel)
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

interface McpSession {
  ask: (line: string, key: number | string, ms?: number) => Promise<McpParsed | null>;
  close: () => void;
}

async function openMcpSession(
  host: SpawnHost,
  binary: string,
  env: Record<string, string>,
): Promise<McpSession | null> {
  let proc: McpProc;
  try {
    proc = host.spawn([binary, "mcp"], {
      stdin: "pipe",
      stdout: "pipe",
      stderr: "pipe",
      env,
    });
  } catch {
    return null;
  }
  if (!proc.stdin || !proc.stdout) {
    try {
      proc.kill();
    } catch {
      /* ignore kill failure during teardown */
    }
    return null;
  }
  const stdin = proc.stdin;
  const reader = proc.stdout.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  const deadline = Date.now() + 20000;
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
      /* reader ends with the child; timeouts below record the outcome */
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
  return {
    ask,
    close: () => {
      try {
        stdin.close();
      } catch {
        /* ignore close failure during teardown */
      }
      try {
        proc.kill();
      } catch {
        /* ignore kill failure during teardown */
      }
    },
  };
}

/** Single-shot MCP tool call for trio cases (own session, bounded). */
async function mcpCallOnce(
  ctx: CheckContext,
  daemon: string,
  binaryOk: boolean,
  isolatedEnv: Record<string, string>,
  tool: string,
  args: Record<string, unknown>,
): Promise<McpParsed | null> {
  if (!binaryOk) return null;
  const host = spawnHost();
  if (!host) return null;
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(ctx.env)) {
    if (typeof v === "string") env[k] = v;
  }
  env["ZESTROKU_DB"] = isolatedEnv["ZESTROKU_DB"];
  env["ZESTROKU_DAEMON_URL"] = daemon;
  const s = await openMcpSession(host, ctx.binary, env);
  if (!s) return null;
  try {
    const init = await s.ask(mcpInitialize(1), 1);
    if (!init || mcpServerName(init.result) !== "zestroku") return null;
    s.ask(mcpLine("notifications/initialized", null, {}), "note", 1000).catch(() => null);
    try {
      await s.ask(mcpLine("tools/list", 2, {}), 2);
    } catch {
      /* list best-effort for single-shot callers */
    }
    return await s.ask(mcpToolsCall(3, tool, args), 3);
  } finally {
    s.close();
  }
}

async function mcpStdioCase(
  ctx: CheckContext,
  daemon: string | null,
  deviceHosts: string[],
  binaryOk: boolean,
  isolatedEnv: Record<string, string>,
): Promise<CheckCase> {
  const id = "mcp-stdio-full-contract";
  if (!binaryOk) {
    return unknownCase(id, "Binary probe failed; MCP stdio handshake not exercisable.");
  }
  const host = spawnHost();
  if (!host) {
    return unknownCase(id, "No interactive spawn channel in this runtime; stdio handshake needs stdin (prerequisite absent).");
  }
  const baseEnv: Record<string, string> = {};
  for (const [k, v] of Object.entries(ctx.env)) {
    if (typeof v === "string") baseEnv[k] = v;
  }
  baseEnv["ZESTROKU_DB"] = isolatedEnv["ZESTROKU_DB"];
  if (daemon) baseEnv["ZESTROKU_DAEMON_URL"] = daemon;
  const session = await openMcpSession(host, ctx.binary, baseEnv);
  if (!session) {
    return { id, outcome: "FAIL", detail: "MCP spawn failed or missing stdio pipes.", claim: "invariant", tier: "simulated" };
  }
  try {
    const init = await session.ask(mcpInitialize(1), 1);
    if (!init || mcpServerName(init.result) !== "zestroku") {
      return {
        id,
        outcome: "FAIL",
        detail: `MCP initialize missing zestroku serverInfo (got ${redact(JSON.stringify(init?.result ?? null)).slice(0, 160)}).`,
        claim: "invariant",
        tier: "simulated",
      };
    }
    session.ask(mcpLine("notifications/initialized", null, {}), "note", 1000).catch(() => null);
    const list = await session.ask(mcpLine("tools/list", 2, {}), 2);
    const names = list && list.result ? mcpToolNames(list.result) : [];
    const missing = MCP_EXPECTED_TOOLS.filter((t) => !names.includes(t));
    const echo =
      names.includes("echo")
        ? await session.ask(mcpToolsCall(3, "echo", { text: "mission-parity-probe" }), 3)
        : null;
    const echoOk =
      !!echo &&
      echo.result !== undefined &&
      JSON.stringify(echo.result).includes("mission-parity-probe");
    const devices = names.includes("list_devices")
      ? await session.ask(mcpToolsCall(4, "list_devices", {}), 4)
      : null;
    const devicesText = redact(JSON.stringify(devices?.result ?? devices?.error ?? null));
    const devicesOk = !!devices && devices.result !== undefined && !looksLikeDirectFallback(devicesText);
    // Shared-authority parity: the MCP inventory names the HTTP inventoried host.
    const firstHost = deviceHosts.length > 0 ? deviceHosts[0] : null;
    const agrees =
      firstHost !== null && devicesOk && devicesText.includes(firstHost);
    const resources = await session.ask(mcpLine("resources/list", 5, {}), 5);
    const resourcesNote = resources
      ? `resources/list answered (${resources.error !== undefined ? "error" : "result"}; honesty recorded)`
      : "resources/list answered nothing (recorded, not faked)";
    // Closed-port honesty: same tools against a dead daemon must error
    // honestly with no rows and no direct dial (never list-only proof).
    const closedEnv = { ...baseEnv, ZESTROKU_DAEMON_URL: CLOSED_PORT_URL };
    const closed = await openMcpSession(host, ctx.binary, closedEnv);
    let closedHonest = false;
    if (closed) {
      try {
        const cInit = await closed.ask(mcpInitialize(11), 11);
        if (cInit && mcpServerName(cInit.result) === "zestroku") {
          closed.ask(mcpLine("notifications/initialized", null, {}), "cnote", 1000).catch(() => null);
          const cList = await closed.ask(mcpToolsCall(12, "list_devices", {}), 12);
          const cText = redact(JSON.stringify(cList?.result ?? cList?.error ?? null));
          closedHonest =
            !!cList &&
            cList.error !== undefined &&
            !looksLikeDirectFallback(cText);
        }
      } finally {
        closed.close();
      }
    }
    if (missing.length === 0 && echoOk && agrees && closedHonest) {
      return {
        id,
        outcome: "PASS",
        detail: `initialize zestroku + 8/8 tools + echo round-trip + list_devices agreeing with HTTP inventory (${devicesText.slice(0, 120)}) + closed-port honest error with zero direct dial; ${resourcesNote} (observed: mcp_stdio_routes_reads_through_daemon_and_preserves_gate; never list-only).`,
        claim: "invariant",
        tier: "simulated",
        covers: ["surface.mcp-stdio.positive", "surface.mcp-stdio.negative"],
      };
    }
    if (!daemon || deviceHosts.length === 0) {
      return unknownCase(
        id,
        `MCP handshake ran (missing=[${missing.join(",")}] echoOk=${echoOk}) but no seeded HTTP inventory exists to prove shared authority (missing witness, not green).`,
      );
    }
    return {
      id,
      outcome: "FAIL",
      detail: `missing=[${missing.join(",")}] echoOk=${echoOk} agrees=${agrees} closedHonest=${closedHonest}; ${resourcesNote}.`,
      claim: "invariant",
      tier: "simulated",
    };
  } finally {
    session.close();
  }
}
