import { isIP } from "node:net";
import type { CheckCase, CheckContext } from "../mission-check";

const commandTimeoutMs = 20 * 60_000;
const requestTimeoutMs = 4_000;

interface Suite {
  packageName: string;
  testTarget: string;
  testName: string;
  covers: readonly string[];
}

interface DeviceRecoveryObservation {
  infoOk: boolean;
  discoveryRecent: boolean;
}

function result(
  id: string,
  outcome: CheckCase["outcome"],
  detail: string,
  claim: CheckCase["claim"] = "invariant",
  tier: CheckCase["tier"] = "fixture",
  covers?: readonly string[],
): CheckCase {
  let safe = "";
  for (const char of detail) {
    const code = char.charCodeAt(0);
    safe += code < 32 || code === 127 ? " " : char;
  }
  const item: CheckCase = { id, outcome, detail: safe.slice(0, 400), claim, tier };
  if (covers) item.covers = covers;
  return item;
}

async function runExistingSuite(ctx: CheckContext, id: string, suite: Suite): Promise<CheckCase> {
  const executor = ctx.env.ZESTROKU_MISSION_RUN_RCH;
  if (executor !== "1" && executor !== "0") {
    return result(id, "UNKNOWN", "Rust test executor was not selected", "invariant", "simulated", suite.covers);
  }
  const runner = executor === "1" ? "remote RCH" : "local cargo";
  try {
    const command = await ctx.runCargoTest(
      ["test", "--locked", "-j1", "-p", suite.packageName, "--test", suite.testTarget, suite.testName, "--", "--exact"],
      { timeoutMs: commandTimeoutMs },
    );
    if (command.stderr === "EXECUTABLE_UNAVAILABLE") {
      return result(id, "UNKNOWN", `${runner} executor is unavailable; exact test did not run`, "invariant", "simulated", suite.covers);
    }
    const lines = `${command.stdout}\n${command.stderr}`.split("\n").map((line) => line.trim());
    const exactPass = lines.includes(`test ${suite.testName} ... ok`);
    const summary = lines.find((line) => line.startsWith("test result: ok."));
    const executed = summary?.includes("1 passed") === true && summary.includes("0 failed");
    const passed = !command.timedOut && command.code === 0 && exactPass && executed;
    const sourceDetail = executor === "1" ? ` on immutable ${ctx.source.slice(0, 12)}` : " in local workspace";
    return result(
      id,
      passed ? "PASS" : "FAIL",
      passed ? `exact ${runner} test ${suite.testName} passed${sourceDetail} with one test executed` : `exact ${runner} test ${suite.testName} did not prove one passing execution (timeout=${command.timedOut}, exit=${command.code})`,
      "invariant",
      "simulated",
      suite.covers,
    );
  } catch {
    return result(id, "FAIL", `${runner} cargo test command ${suite.testName} could not execute`, "invariant", "simulated", suite.covers);
  }
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

function targetHost(target: string): string {
  if (target.startsWith("[")) {
    const end = target.indexOf("]");
    return end < 0 ? "" : target.slice(1, end);
  }
  if (isIP(target) === 6) return target;
  const split = target.lastIndexOf(":");
  if (split <= 0) return target;
  const port = target.slice(split + 1);
  if (port.length === 0 || !Array.from(port).every((char) => char >= "0" && char <= "9")) return target;
  return target.slice(0, split);
}

function isLoopback(host: string): boolean {
  const stripped = host.startsWith("[") && host.endsWith("]") ? host.slice(1, -1) : host;
  const normalized = stripped.toLowerCase();
  return normalized === "::1" || normalized === "::ffff:127.0.0.1" || normalized.startsWith("127.") || normalized === "localhost";
}

async function liveJson(url: URL): Promise<{ status: number; body: unknown }> {
  const response = await fetch(url, { signal: AbortSignal.timeout(requestTimeoutMs) });
  const text = await response.text();
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    throw new Error(`HTTP ${response.status} returned non-JSON`);
  }
  return { status: response.status, body };
}

function serviceBase(ctx: CheckContext): URL | undefined {
  const configured = ctx.env.ZESTROKU_MISSION_SERVICE_URL;
  if (!configured) return undefined;
  const base = new URL(configured);
  if (base.protocol !== "http:" && base.protocol !== "https:") throw new Error("service URL must use HTTP or HTTPS");
  if (isLoopback(base.hostname)) throw new Error("live service URL must be non-loopback");
  return base;
}

export async function probeDeviceRecovery(
  base: URL,
  target: string,
  maxStaleSeconds: number,
  nowSeconds = Math.floor(Date.now() / 1000),
): Promise<DeviceRecoveryObservation> {
  // Capture the inventory timestamp before the info request can refresh state.
  const inventoryResponse = await liveJson(new URL("/v1/devices", base));
  let discoveryRecent = false;
  if (inventoryResponse.status === 200 && Array.isArray(inventoryResponse.body)) {
    discoveryRecent = inventoryResponse.body.some((entry: unknown) => {
      if (!entry || typeof entry !== "object" || !("host" in entry) || !("last_seen" in entry)) return false;
      const host = entry.host;
      const lastSeen = entry.last_seen;
      if (host !== targetHost(target) || typeof lastSeen !== "number") return false;
      const age = nowSeconds - lastSeen;
      return age >= 0 && age <= maxStaleSeconds;
    });
  }

  const infoResponse = await liveJson(new URL(`/v1/devices/${encodeURIComponent(target)}/info`, base));
  let infoOk = false;
  const info = infoResponse.body;
  if (info && typeof info === "object" && "access_tier" in info && "model_number" in info) {
    const tier = info.access_tier;
    const model = info.model_number;
    infoOk = infoResponse.status === 200 && typeof tier === "string" && tier !== "unknown" && typeof model === "string" && model.length > 0;
  }
  return { infoOk, discoveryRecent };
}

export function evaluateDeviceRecovery(observation: DeviceRecoveryObservation): "PASS" | "FAIL" {
  return observation.infoOk && observation.discoveryRecent ? "PASS" : "FAIL";
}

async function physicalRead(ctx: CheckContext, base: URL, envName: string, id: string): Promise<CheckCase> {
  const target = ctx.env[envName];
  if (!target) {
    return result(id, "UNKNOWN", `${envName} is not configured; physical-device witness is absent`, "live-witness", "live", ["live.device-witness.positive"]);
  }
  const host = targetHost(target);
  if (isIP(host) === 0 || isLoopback(host)) {
    return result(id, "UNKNOWN", `${envName} must identify a non-loopback IP endpoint; simulator/hostname evidence is not physical-device proof`, "live-witness", "live", ["live.device-witness.positive"]);
  }
  const maxAge = staleBound(ctx.env.ZESTROKU_MISSION_MAX_STALE_SECONDS);
  if (maxAge === undefined) {
    return result(id, "UNKNOWN", "ZESTROKU_MISSION_MAX_STALE_SECONDS is unset or invalid; discovery-recency bound is unspecified", "live-witness", "live", ["live.device-witness.positive"]);
  }
  try {
    const observation = await probeDeviceRecovery(base, target, maxAge);
    const outcome = evaluateDeviceRecovery(observation);
    return result(
      id,
      outcome,
      outcome === "PASS" ? "configured non-loopback device IP returned live info and a recent discovery timestamp" : `device witness failed: infoOk=${observation.infoOk}, discoveryRecent=${observation.discoveryRecent}`,
      "live-witness",
      "live",
      ["live.device-witness.positive"],
    );
  } catch {
    return result(id, "FAIL", "configured physical-device info request failed or timed out", "live-witness", "live", ["live.device-witness.positive"]);
  }
}

const requiredSuites: Array<[string, Suite]> = [
  ["recovery.identity-move", { packageName: "zestroku-cli", testTarget: "devices_store", testName: "moved_device_keeps_one_row_and_aliases_follow", covers: ["cap.identity.positive", "surface.simulator.positive"] }],
  ["recovery.identity-no-merge", { packageName: "zestroku-cli", testTarget: "devices_store", testName: "same_model_different_identities_never_merge", covers: ["cap.identity.negative", "surface.simulator.negative"] }],
  ["recovery.identity-unknown", { packageName: "zestroku-cli", testTarget: "devices_store", testName: "unknown_identity_same_model_devices_never_merge", covers: ["cap.identity.negative", "surface.simulator.negative"] }],
  ["recovery.suspend-resume", { packageName: "zestroku-cli", testTarget: "devices_scan", testName: "scan_tolerates_suspended_sim_then_finds_it_after_resume", covers: ["cap.recovery.positive", "cap.recovery.negative", "surface.simulator.positive", "surface.simulator.negative"] }],
  ["recovery.tier-permissive", { packageName: "zestroku-cli", testTarget: "info_tier", testName: "info_prints_and_records_permissive_on_the_express", covers: ["cap.recovery.positive"] }],
  ["recovery.tier-limited", { packageName: "zestroku-cli", testTarget: "info_tier", testName: "info_prints_and_records_limited_on_a_limited_device", covers: ["cap.recovery.negative"] }],
  ["recovery.sigint", { packageName: "zestroku-cli", testTarget: "serve_readiness", testName: "sigint_stops_serve_after_health_readiness_and_releases_listener", covers: ["cap.recovery.positive"] }],
  ["recovery.concurrent-drain", { packageName: "zestroku-cli", testTarget: "serve_drain_controls", testName: "c2_inflight_proxy_held_info_completes_after_term", covers: ["cap.recovery.positive"] }],
];

export async function run(ctx: CheckContext): Promise<CheckCase[]> {
  if (!fullCommitSha(ctx.source)) {
    return [
      result("recovery.source", "FAIL", "source must be a full immutable 40-character commit SHA"),
      ...requiredSuites.map(([id, suite]) => result(id, "UNKNOWN", "source is not pinned; remote suite was not run", "invariant", "simulated", suite.covers)),
      result("recovery.mutation-no-retry", "UNKNOWN", "no existing test proves no automatic retry after a failed mutation dispatch"),
      result("recovery.concurrent-unavailable-shutdown", "UNKNOWN", "no existing immutable-source test couples unavailable ECP with concurrent health and shutdown"),
      result("recovery.live-device-negative", "UNKNOWN", "a real physical-device negative witness is not configured", "live-witness", "live", ["live.device-witness.negative"]),
      result("recovery.live-mutation-witness", "UNKNOWN", "real device mutation is prohibited in this read-only checker", "live-witness", "live", ["live.approved-mutation-witness.positive", "live.approved-mutation-witness.negative"]),
      result("recovery.live-tailnet-negative", "UNKNOWN", "tailnet negative witness is not configured", "live-witness", "live", ["live.tailnet-witness.negative"]),
      result("recovery.live-express", "UNKNOWN", "source is not pinned; live check was not run", "live-witness", "live", ["live.device-witness.positive"]),
      result("recovery.live-tv", "UNKNOWN", "source is not pinned; live check was not run", "live-witness", "live", ["live.device-witness.positive"]),
      result("recovery.tailnet", "UNKNOWN", "source is not pinned; live check was not run", "live-witness", "live", ["live.tailnet-witness.positive"]),
    ];
  }

  const cases: CheckCase[] = [result("recovery.source", "PASS", `checks pinned to ${ctx.source}`)];
  for (const [id, suite] of requiredSuites) cases.push(await runExistingSuite(ctx, id, suite));
  cases.push(result("recovery.mutation-no-retry", "UNKNOWN", "no existing test proves no automatic retry after a failed mutation dispatch"));
  cases.push(result("recovery.concurrent-unavailable-shutdown", "UNKNOWN", "existing drain test covers a held ECP response, not an unavailable endpoint during health and SIGINT; no equivalent behavior is claimed"));
  cases.push(result("recovery.live-device-negative", "UNKNOWN", "a real physical-device negative witness is not configured", "live-witness", "live", ["live.device-witness.negative"]));
  cases.push(result("recovery.live-mutation-witness", "UNKNOWN", "real device mutation is prohibited in this read-only checker", "live-witness", "live", ["live.approved-mutation-witness.positive", "live.approved-mutation-witness.negative"]));
  cases.push(result("recovery.live-tailnet-negative", "UNKNOWN", "tailnet negative witness is not configured", "live-witness", "live", ["live.tailnet-witness.negative"]));

  let base: URL | undefined;
  try {
    base = serviceBase(ctx);
  } catch {
    cases.push(result("recovery.live-service-url", "UNKNOWN", "configured live service URL is invalid or loopback", "live-witness", "live"));
  }
  if (!base) {
    cases.push(result("recovery.live-express", "UNKNOWN", "ZESTROKU_MISSION_SERVICE_URL is not configured as a non-loopback service URL", "live-witness", "live", ["live.device-witness.positive"]));
    cases.push(result("recovery.live-tv", "UNKNOWN", "ZESTROKU_MISSION_SERVICE_URL is not configured as a non-loopback service URL", "live-witness", "live", ["live.device-witness.positive"]));
    cases.push(result("recovery.tailnet", "UNKNOWN", "off-LAN tailnet consumer witness is not configured", "live-witness", "live", ["live.tailnet-witness.positive"]));
    return cases;
  }

  cases.push(await physicalRead(ctx, base, "ZESTROKU_MISSION_EXPRESS_TARGET", "recovery.live-express"));
  cases.push(await physicalRead(ctx, base, "ZESTROKU_MISSION_TV_TARGET", "recovery.live-tv"));
  const tailnet = ctx.env.ZESTROKU_MISSION_TAILNET_URL;
  if (!tailnet) {
    cases.push(result("recovery.tailnet", "UNKNOWN", "off-LAN tailnet consumer witness is not configured", "live-witness", "live", ["live.tailnet-witness.positive"]));
  } else {
    cases.push(result("recovery.tailnet", "UNKNOWN", "a same-process probe cannot prove an authorized off-LAN tailnet consumer workflow", "live-witness", "live", ["live.tailnet-witness.positive"]));
  }
  return cases;
}
