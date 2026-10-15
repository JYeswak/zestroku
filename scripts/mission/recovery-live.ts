import type { CheckCase, CheckContext } from "../mission-check";

const commandTimeoutMs = 20 * 60_000;
const requestTimeoutMs = 4_000;

interface Suite { packageName: string; testTarget: string; testName: string; expected: string[] }

function result(id: string, outcome: CheckCase["outcome"], detail: string, claim: CheckCase["claim"] = "invariant", tier: CheckCase["tier"] = "fixture"): CheckCase {
  let safe = "";
  for (const char of detail) {
    const code = char.charCodeAt(0);
    safe += code < 32 || code === 127 ? " " : char;
  }
  return { id, outcome, detail: safe.slice(0, 400), claim, tier };
}

async function runExistingSuite(ctx: CheckContext, id: string, suite: Suite): Promise<CheckCase> {
  const command = await ctx.exec(["rch", "exec", "--base", ctx.source, "--clean-overlay", "--no-overlay", "--", "cargo", "test", "--locked", "-p", suite.packageName, "--test", suite.testTarget, suite.testName, "--", "--exact"], { env: ctx.env, timeoutMs: commandTimeoutMs });
  const output = `${command.stdout}\n${command.stderr}`;
  const passed = !command.timedOut && command.code === 0 && suite.expected.every((part) => output.includes(part));
  return result(id, passed ? "PASS" : "FAIL", passed ? `immutable ${ctx.source.slice(0, 12)} remote test ${suite.testName} passed` : `immutable remote test ${suite.testName} did not prove expected assertions (timeout=${command.timedOut}, exit=${command.code})`, "invariant", "simulated");
}

function fullCommitSha(value: string): boolean {
  if (value.length !== 40) return false;
  for (const char of value) {
    const code = char.toLowerCase().charCodeAt(0);
    if (!((code >= 48 && code <= 57) || (code >= 97 && code <= 102))) return false;
  }
  return true;
}

function staleBound(value: string | undefined): number | undefined {
  if (!value || value.length > 6) return undefined;
  let number = 0;
  for (const char of value) {
    const digit = char.charCodeAt(0) - 48;
    if (digit < 0 || digit > 9) return undefined;
    number = number * 10 + digit;
  }
  return number;
}

async function liveJson(url: URL): Promise<{ status: number; body: unknown }> {
  const response = await fetch(url, { signal: AbortSignal.timeout(requestTimeoutMs) });
  const text = await response.text();
  let body: unknown;
  try { body = JSON.parse(text); } catch { throw new Error(`HTTP ${response.status} returned non-JSON`); }
  return { status: response.status, body };
}

function serviceBase(ctx: CheckContext): URL | undefined {
  const configured = ctx.env.ZESTROKU_MISSION_SERVICE_URL;
  if (!configured) return undefined;
  const base = new URL(configured);
  if (base.protocol !== "http:" && base.protocol !== "https:") throw new Error("service URL must use HTTP or HTTPS");
  return base;
}

export async function probeDeviceRecovery(base: URL, target: string, maxStaleSeconds: number, nowSeconds = Math.floor(Date.now() / 1000)): Promise<{ infoOk: boolean; inventoryFresh: boolean }> {
  const infoResponse = await liveJson(new URL(`/v1/devices/${encodeURIComponent(target)}/info`, base));
  let infoOk = false;
  const info = infoResponse.body;
  if (info && typeof info === "object" && "access_tier" in info && "model_number" in info) {
    const tier = info.access_tier;
    const model = info.model_number;
    infoOk = infoResponse.status === 200 && typeof tier === "string" && tier !== "unknown" && typeof model === "string" && model.length > 0;
  }

  const inventoryResponse = await liveJson(new URL("/v1/devices", base));
  let inventoryFresh = false;
  if (inventoryResponse.status === 200 && Array.isArray(inventoryResponse.body)) {
    inventoryFresh = inventoryResponse.body.some((entry: unknown) => {
      if (!entry || typeof entry !== "object" || !("host" in entry) || !("last_seen" in entry)) return false;
      const host = entry.host;
      const lastSeen = entry.last_seen;
      if (host !== target.split(":")[0] || typeof lastSeen !== "number") return false;
      const age = nowSeconds - lastSeen;
      return age >= 0 && age <= maxStaleSeconds;
    });
  }
  return { infoOk, inventoryFresh };
}

async function physicalRead(ctx: CheckContext, base: URL, envName: string, id: string): Promise<CheckCase> {
  const target = ctx.env[envName];
  if (!target) return result(id, "UNKNOWN", `${envName} is not configured; physical-device acceptance not observed`, "live-witness", "live");
  const maxAge = staleBound(ctx.env.ZESTROKU_MISSION_MAX_STALE_SECONDS);
  if (maxAge === undefined) return result(id, "UNKNOWN", "ZESTROKU_MISSION_MAX_STALE_SECONDS is unset or invalid; freshness boundary is not configured", "live-witness", "live");
  try {
    const state = await probeDeviceRecovery(base, target, maxAge);
    const outcome = state.infoOk && state.inventoryFresh ? "PASS" : "FAIL";
    return result(id, outcome, outcome === "PASS" ? "live info is reachable and inventory last_seen is within the configured freshness bound" : `live recovery failed: infoOk=${state.infoOk}, inventoryFresh=${state.inventoryFresh}`, "live-witness", "live");
  } catch {
    return result(id, "FAIL", "configured physical-device recovery request failed or timed out", "live-witness", "live");
  }
}

const requiredSuites: Array<[string, Suite]> = [
  ["recovery.identity-move", { packageName: "zestroku-cli", testTarget: "devices_store", testName: "moved_device_keeps_one_row_and_aliases_follow", expected: ["1 passed", "0 failed"] }],
  ["recovery.identity-no-merge", { packageName: "zestroku-cli", testTarget: "devices_store", testName: "same_model_different_identities_never_merge", expected: ["1 passed", "0 failed"] }],
  ["recovery.identity-unknown", { packageName: "zestroku-cli", testTarget: "devices_store", testName: "unknown_identity_same_model_devices_never_merge", expected: ["1 passed", "0 failed"] }],
  ["recovery.suspend-resume", { packageName: "zestroku-cli", testTarget: "devices_scan", testName: "scan_tolerates_suspended_sim_then_finds_it_after_resume", expected: ["1 passed", "0 failed"] }],
  ["recovery.tier-permissive", { packageName: "zestroku-cli", testTarget: "info_tier", testName: "info_prints_and_records_permissive_on_the_express", expected: ["1 passed", "0 failed"] }],
  ["recovery.tier-limited", { packageName: "zestroku-cli", testTarget: "info_tier", testName: "info_prints_and_records_limited_on_a_limited_device", expected: ["1 passed", "0 failed"] }],
  ["recovery.sigint", { packageName: "zestroku-cli", testTarget: "serve_readiness", testName: "sigint_stops_serve_after_health_readiness_and_releases_listener", expected: ["1 passed", "0 failed"] }],
  ["recovery.mutation-no-retry", { packageName: "zestroku-cli", testTarget: "mutation_api", testName: "real_http_mutations_use_inventory_bound_approval_snapshot_and_audit", expected: ["1 passed", "0 failed"] }],
  ["recovery.concurrent-drain", { packageName: "zestroku-cli", testTarget: "serve_drain_controls", testName: "c2_inflight_proxy_held_info_completes_after_term", expected: ["1 passed", "0 failed"] }],
];

export async function run(ctx: CheckContext): Promise<CheckCase[]> {
  if (!fullCommitSha(ctx.source)) {
    return [
      result("recovery.source", "FAIL", "source must be a full immutable 40-character commit SHA"),
      ...requiredSuites.map(([id]) => result(id, "UNKNOWN", "source is not pinned; remote suite was not run")),
      result("recovery.concurrent-unavailable-shutdown", "UNKNOWN", "no existing immutable-source test couples unavailable ECP with concurrent health and shutdown"),
      result("recovery.live-express", "UNKNOWN", "source is not pinned; live check was not run", "live-witness", "live"),
      result("recovery.live-tv", "UNKNOWN", "source is not pinned; live check was not run", "live-witness", "live"),
      result("recovery.tailnet", "UNKNOWN", "source is not pinned; live check was not run", "live-witness", "live"),
    ];
  }

  const cases: CheckCase[] = [result("recovery.source", "PASS", `checks pinned to ${ctx.source}`)];
  for (const [id, suite] of requiredSuites) cases.push(await runExistingSuite(ctx, id, suite));
  cases.push(result("recovery.concurrent-unavailable-shutdown", "UNKNOWN", "existing drain test covers a held ECP response, not an unavailable endpoint during health and SIGINT; no equivalent behavior is claimed"));

  let base: URL | undefined;
  try { base = serviceBase(ctx); } catch { cases.push(result("recovery.live-service-url", "FAIL", "configured service URL is invalid", "live-witness", "live")); }
  if (!base) {
    cases.push(result("recovery.live-express", "UNKNOWN", "ZESTROKU_MISSION_SERVICE_URL is not configured", "live-witness", "live"));
    cases.push(result("recovery.live-tv", "UNKNOWN", "ZESTROKU_MISSION_SERVICE_URL is not configured", "live-witness", "live"));
    cases.push(result("recovery.tailnet", "UNKNOWN", "ZESTROKU_MISSION_TAILNET_URL is not configured", "live-witness", "live"));
    return cases;
  }

  cases.push(await physicalRead(ctx, base, "ZESTROKU_MISSION_EXPRESS_TARGET", "recovery.live-express"));
  cases.push(await physicalRead(ctx, base, "ZESTROKU_MISSION_TV_TARGET", "recovery.live-tv"));
  const tailnet = ctx.env.ZESTROKU_MISSION_TAILNET_URL;
  if (!tailnet) {
    cases.push(result("recovery.tailnet", "UNKNOWN", "ZESTROKU_MISSION_TAILNET_URL is not configured", "live-witness", "live"));
  } else {
    try {
      const health = await liveJson(new URL("/health", tailnet));
      const body = health.body;
      const ok = health.status === 200 && body !== null && typeof body === "object" && "status" in body && body.status === "ok";
      cases.push(result("recovery.tailnet", ok ? "PASS" : "FAIL", ok ? "configured tailnet endpoint returned healthy status" : `tailnet health contract failed (HTTP ${health.status})`, "live-witness", "live"));
    } catch {
      cases.push(result("recovery.tailnet", "FAIL", "configured tailnet health request failed or timed out", "live-witness", "live"));
    }
  }
  return cases;
}
