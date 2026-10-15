import type { CheckCase, CheckContext } from "../mission-check";
import { createServer as createNetServer } from "node:net";
import { createServer as createHttpServer } from "node:http";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";

const timeoutMs = 5_000;
const fixtureInfo = "<device-info><model-name>Mission Fixture</model-name><model-number>FIXTURE-1</model-number><software-version>1.0</software-version><is-tv>false</is-tv></device-info>";

function result(
  id: string,
  outcome: CheckCase["outcome"],
  detail: string,
  claim: CheckCase["claim"] = "invariant",
  tier: CheckCase["tier"] = "fixture",
): CheckCase {
  return { id, outcome, detail, claim, tier };
}

export function assessHealth(status: number, body: unknown): boolean {
  return status === 200 && typeof body === "object" && body !== null && "status" in body && body.status === "ok";
}

export function assessInventory(status: number, body: unknown, expectedHost: string): boolean {
  if (status !== 200 || !Array.isArray(body) || expectedHost.trim().length === 0) return false;
  return body.some((row) => row !== null && typeof row === "object" &&
    "host" in row && row.host === expectedHost &&
    "name" in row && typeof row.name === "string" &&
    "model" in row && typeof row.model === "string" &&
    "model_number" in row && typeof row.model_number === "string" &&
    "software_version" in row && typeof row.software_version === "string" &&
    "access_tier" in row && typeof row.access_tier === "string" &&
    "last_seen" in row && typeof row.last_seen === "number" && Number.isSafeInteger(row.last_seen));
}

async function fetchJson(url: string): Promise<{ status: number; body: unknown }> {
  const response = await fetch(url, { signal: AbortSignal.timeout(timeoutMs) });
  const text = await response.text();
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    throw new Error(`non-JSON response (HTTP ${response.status})`);
  }
  return { status: response.status, body };
}

function checkedServiceUrl(value: string): URL {
  const base = new URL(value);
  if (base.protocol !== "http:" || base.username || base.password) {
    throw new Error("service URL must be plain HTTP without URL credentials");
  }
  return base;
}

export async function inspectHealth(url: string): Promise<CheckCase> {
  try {
    const response = await fetchJson(url);
    const healthy = assessHealth(response.status, response.body);
    return result(
      "resident.http-health",
      healthy ? "PASS" : "FAIL",
      healthy ? "reachable endpoint returned HTTP 200 with status ok" : `reachable endpoint violated health contract with HTTP ${response.status}`,
      "invariant",
      "fixture",
    );
  } catch {
    return result("resident.http-health", "FAIL", "configured service health request failed or returned invalid JSON", "invariant", "fixture");
  }
}

async function inspectInventory(url: string, expectedHost: string | undefined): Promise<CheckCase> {
  if (!expectedHost) return result("resident.http-inventory", "UNKNOWN", "expected device host is absent; no target-specific inventory assertion was made", "live-witness", "live");
  try {
    const response = await fetchJson(url);
    const found = assessInventory(response.status, response.body, expectedHost);
    return result(
      "resident.http-inventory",
      found ? "PASS" : "FAIL",
      found ? "reachable endpoint returned the expected device with all required inventory fields" : `reachable endpoint omitted the expected device or violated the inventory schema with HTTP ${response.status}`,
      "invariant",
      "fixture",
    );
  } catch {
    return result("resident.http-inventory", "FAIL", "configured service inventory request failed or returned invalid JSON", "invariant", "fixture");
  }
}

async function firstRunDiscovery(ctx: CheckContext): Promise<CheckCase> {
  const requests: string[] = [];
  const server = createHttpServer((request, response) => {
    requests.push(`${request.method ?? ""} ${request.url ?? ""}`);
    if (request.method === "GET" && request.url === "/query/device-info") {
      response.writeHead(200, { "content-type": "application/xml" });
      response.end(fixtureInfo);
      return;
    }
    response.writeHead(404);
    response.end();
  });
  try {
    const listening = Promise.withResolvers<void>();
    server.once("error", listening.reject);
    server.listen(0, "127.0.0.1", listening.resolve);
    await listening.promise;
    const address = server.address();
    if (!address || typeof address === "string") return result("store.first-run-discovery", "UNKNOWN", "loopback fixture did not expose a port", "invariant", "simulated");

    const targets = join(ctx.scratch, "first-run-targets.toml");
    await writeFile(targets, `[[target]]\nhost = "127.0.0.1"\nport = ${address.port}\n`, { encoding: "utf8", mode: 0o600 });
    const output = await ctx.exec([ctx.binary, "devices", "--no-ssdp", "--targets", targets, "--json"], {
      env: { ...ctx.env, HOME: join(ctx.scratch, "first-run-home"), ZESTROKU_DB: join(ctx.scratch, "first-run", "actions.db") },
      timeoutMs,
    });
    let document: unknown;
    try {
      document = JSON.parse(output.stdout);
    } catch {
      document = null;
    }
    let rows: unknown[] = [];
    if (document && typeof document === "object" && "devices" in document && Array.isArray(document.devices)) rows = document.devices;
    const modeledRow = rows.some((row) => row !== null && typeof row === "object" &&
      "model_name" in row && row.model_name === "Mission Fixture" &&
      "model_number" in row && row.model_number === "FIXTURE-1" &&
      "software_version" in row && row.software_version === "1.0");
    const observedRequest = requests.includes("GET /query/device-info");
    const passed = !output.timedOut && output.code === 0 && modeledRow && observedRequest;
    return result(
      "store.first-run-discovery",
      passed ? "PASS" : "FAIL",
      passed ? "real CLI discovered the loopback fixture from an empty isolated store and completed its inventory write" : `first-run fixture contract failed: code=${output.code}, timedOut=${output.timedOut}, modeledRow=${modeledRow}, deviceInfoRequest=${observedRequest}`,
      "invariant",
      "simulated",
    );
  } catch {
    return result("store.first-run-discovery", "UNKNOWN", "isolated CLI discovery fixture could not be prepared", "invariant", "simulated");
  } finally {
    if (server.listening) await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

async function bindNegative(ctx: CheckContext): Promise<CheckCase> {
  const server = createNetServer();
  try {
    const listening = Promise.withResolvers<void>();
    server.once("error", listening.reject);
    server.listen(0, "127.0.0.1", listening.resolve);
    await listening.promise;
    const address = server.address();
    if (!address || typeof address === "string") {
      return result("resident.bind-conflict", "FAIL", "could not obtain an occupied loopback port", "invariant", "simulated");
    }
    const attempted = await ctx.exec(
      [ctx.binary, "serve", "--bind", `127.0.0.1:${address.port}`],
      {
        env: { ...ctx.env, ZESTROKU_DB: join(ctx.scratch, "bind-negative.db") },
        timeoutMs,
      },
    );
    const output = `${attempted.stdout} ${attempted.stderr}`.toLowerCase();
    const rejected = !attempted.timedOut && attempted.code !== 0 &&
      ["address already in use", "os error 48", "os error 98"].some((message) => output.includes(message));
    return result(
      "resident.bind-conflict",
      rejected ? "PASS" : "FAIL",
      rejected ? "real CLI startup rejected an occupied loopback port" : `expected bind-conflict refusal; code=${attempted.code}, timedOut=${attempted.timedOut}`,
      "invariant",
      "simulated",
    );
  } catch {
    return result("resident.bind-conflict", "UNKNOWN", "occupied-port CLI witness unavailable", "invariant", "simulated");
  } finally {
    if (server.listening) await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

export async function run(ctx: CheckContext): Promise<CheckCase[]> {
  const cases: CheckCase[] = [];
  const version = await ctx.exec([ctx.binary, "--version"], { env: ctx.env, timeoutMs });
  if (version.timedOut || version.code !== 0) {
    const unavailable = version.timedOut ? "version command timed out" : `version command exited ${version.code}`;
    for (const id of ["cli.version-revision-hint", "cli.root-help", "cli.serve-help", "cli.invalid-command", "store.first-run-discovery"]) {
      cases.push(result(id, "UNKNOWN", `installed CLI unavailable: ${unavailable}`, "invariant", "live"));
    }
  } else {
    const versionText = version.stdout.trim();
    const revisionHint = ctx.source.length >= 7 && versionText.includes(ctx.source.slice(0, 7)) && !versionText.includes("-dirty");
    cases.push(result(
      "cli.version-revision-hint",
      revisionHint ? "PASS" : "FAIL",
      revisionHint ? "CLI version contains the supplied source's abbreviated revision; this is not build provenance" : "CLI version does not match the supplied source's abbreviated revision or reports dirty state",
      "invariant",
      "fixture",
    ));

    const rootHelp = await ctx.exec([ctx.binary, "--help"], { env: ctx.env, timeoutMs });
    const rootHelpText = `${rootHelp.stdout}\n${rootHelp.stderr}`;
    const rootOk = !rootHelp.timedOut && rootHelp.code === 0 && rootHelpText.includes("USAGE:") && rootHelpText.includes("serve");
    cases.push(result("cli.root-help", rootOk ? "PASS" : "FAIL", rootOk ? "root help rendered with success status" : `root help contract failed: code=${rootHelp.code}, timedOut=${rootHelp.timedOut}`, "invariant", "fixture"));

    const subHelp = await ctx.exec([ctx.binary, "serve", "--help"], { env: ctx.env, timeoutMs });
    const subHelpText = `${subHelp.stdout}\n${subHelp.stderr}`;
    const subOk = !subHelp.timedOut && subHelp.code === 0 && subHelpText.includes("--bind");
    cases.push(result("cli.serve-help", subOk ? "PASS" : "FAIL", subOk ? "serve help rendered with success status" : `serve help contract failed: code=${subHelp.code}, timedOut=${subHelp.timedOut}`, "invariant", "fixture"));

    const invalid = await ctx.exec([ctx.binary, "__mission_invalid_command__"], { env: ctx.env, timeoutMs });
    const invalidOk = !invalid.timedOut && invalid.code === 2 && invalid.stderr.includes("USAGE:");
    cases.push(result("cli.invalid-command", invalidOk ? "PASS" : "FAIL", invalidOk ? "unknown command failed with usage diagnostics" : `invalid command contract failed: code=${invalid.code}, timedOut=${invalid.timedOut}`, "invariant", "fixture"));
    cases.push(await firstRunDiscovery(ctx));
  }

  const serviceUrl = ctx.env.ZESTROKU_MISSION_SERVICE_URL;
  if (!serviceUrl) {
    cases.push(result("resident.http-health", "UNKNOWN", "service URL is absent; resident HTTP health was not observed", "live-witness", "live"));
    cases.push(result("resident.http-inventory", "UNKNOWN", "service URL is absent; target-specific inventory was not observed", "live-witness", "live"));
  } else {
    try {
      const base = checkedServiceUrl(serviceUrl);
      cases.push(await inspectHealth(new URL("/health", base).toString()));
      cases.push(await inspectInventory(new URL("/v1/devices", base).toString(), ctx.env.ZESTROKU_MISSION_EXPECTED_DEVICE_HOST));
    } catch {
      cases.push(result("resident.http-health", "FAIL", "configured service URL is invalid", "invariant", "fixture"));
      cases.push(result("resident.http-inventory", "FAIL", "configured service URL is invalid", "invariant", "fixture"));
    }
  }
  cases.push(await bindNegative(ctx));
  cases.push(result("surface.setup", "UNKNOWN", "owner-facing setup messaging and post-discovery first usable read were not exercised", "live-witness", "live"));
  cases.push(result("surface.store", "UNKNOWN", "durable readback after restart and migration behavior were not exercised", "live-witness", "live"));
  cases.push(result("surface.resident", "UNKNOWN", "launchd install, restart, crash recovery, and running-service source binding were not exercised", "live-witness", "live"));
  cases.push(result("surface.doctor", "UNKNOWN", "no installed doctor workflow was exercised", "live-witness", "live"));
  return cases;
}
