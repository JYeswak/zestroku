import type { CheckCase, CheckContext } from "../mission-check";
import { createServer } from "node:net";

const timeoutMs = 5_000;

function result(
  id: string,
  outcome: CheckCase["outcome"],
  detail: string,
  claim: CheckCase["claim"] = "invariant",
  tier: CheckCase["tier"] = "fixture",
): CheckCase {
  return { id, outcome, detail, claim, tier };
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

async function bindNegative(ctx: CheckContext): Promise<CheckCase> {
  const server = createServer();
  try {
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", () => resolve());
    });
    const address = server.address();
    if (!address || typeof address === "string") {
      return result("resident.bind-conflict", "FAIL", "could not obtain an occupied loopback port");
    }
    const attempted = await ctx.exec(
      [ctx.binary, "serve", "--bind", `127.0.0.1:${address.port}`],
      {
        env: { ...ctx.env, ZESTROKU_DB: `${ctx.scratch}/bind-negative.db` },
        timeoutMs,
      },
    );
    const output = `${attempted.stdout} ${attempted.stderr}`.toLowerCase();
    const rejected = !attempted.timedOut && attempted.code !== 0 &&
      ["address already in use", "os error 48", "os error 98"].some((message) => output.includes(message));
    return result(
      "resident.bind-conflict",
      rejected ? "PASS" : "FAIL",
      rejected ? "real serve startup rejected an occupied loopback port" : `expected nonzero bind-conflict refusal; code=${attempted.code}, timedOut=${attempted.timedOut}`,
      "invariant",
      "simulated",
    );
  } catch {
    return result("resident.bind-conflict", "FAIL", "could not execute the real occupied-port startup check", "invariant", "simulated");
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

export async function run(ctx: CheckContext): Promise<CheckCase[]> {
  const cases: CheckCase[] = [];
  const source = ctx.source.trim();
  const validRevision = source.length >= 40 && source.length <= 64 && [...source].every((character) => "0123456789abcdefABCDEF".includes(character));
  const revision = validRevision ? source.slice(0, 7) : "";
  const version = await ctx.exec([ctx.binary, "--version"], { env: ctx.env, timeoutMs });
  if (version.timedOut || version.code !== 0) {
    const unavailable = version.timedOut ? "version command timed out" : `version command exited ${version.code}`;
    for (const id of ["cli.source-bound-artifact", "cli.root-help", "cli.subcommand-help", "store.first-run", "resident.http-health", "resident.http-inventory"]) {
      cases.push(result(id, "UNKNOWN", `required installed CLI unavailable: ${unavailable}`));
    }
    cases.push(result("resident.bind-conflict", "UNKNOWN", "installed CLI unavailable; startup bind-negative was not exercised"));
    return cases;
  }

  const versionText = `${version.stdout} ${version.stderr}`.trim();
  const attestedSource = ctx.env.ZESTROKU_MISSION_ARTIFACT_SOURCE?.trim();
  const sourceBound = revision !== "" && attestedSource === source && versionText.includes(revision) && !versionText.includes("-dirty");
  const sourceOutcome: CheckCase["outcome"] = !attestedSource ? "UNKNOWN" : sourceBound ? "PASS" : "FAIL";
  cases.push(result(
    "cli.source-bound-artifact",
    sourceOutcome,
    sourceOutcome === "PASS" ? `installed binary version and artifact attestation identify source ${revision}` : sourceOutcome === "UNKNOWN" ? "artifact source attestation is absent" : "artifact attestation, supplied source revision, and clean binary version disagree",
  ));

  const rootHelp = await ctx.exec([ctx.binary, "--help"], { env: ctx.env, timeoutMs });
  const rootHelpText = `${rootHelp.stdout}\n${rootHelp.stderr}`;
  const rootOk = !rootHelp.timedOut && rootHelp.code === 0 && rootHelpText.includes("USAGE:") && rootHelpText.includes("serve");
  cases.push(result("cli.root-help", rootOk ? "PASS" : "FAIL", rootOk ? "root help rendered with success status" : `root help contract failed: code=${rootHelp.code}, timedOut=${rootHelp.timedOut}`));

  const subHelp = await ctx.exec([ctx.binary, "serve", "--help"], { env: ctx.env, timeoutMs });
  const subHelpText = `${subHelp.stdout}\n${subHelp.stderr}`;
  const subOk = !subHelp.timedOut && subHelp.code === 0 && subHelpText.includes("--bind");
  cases.push(result("cli.subcommand-help", subOk ? "PASS" : "FAIL", subOk ? "serve help rendered with success status" : `serve help contract failed: code=${subHelp.code}, timedOut=${subHelp.timedOut}`));

  const firstRun = await ctx.exec([ctx.binary, "alias", "ls"], {
    env: { ...ctx.env, HOME: `${ctx.scratch}/first-run-home`, ZESTROKU_DB: `${ctx.scratch}/first-run/actions.db` },
    timeoutMs,
  });
  const firstRunOk = !firstRun.timedOut && firstRun.code === 0;
  cases.push(result("store.first-run", firstRunOk ? "PASS" : "FAIL", firstRunOk ? "empty isolated store opened through the real CLI without pre-seeded device state" : `empty-store CLI path failed: code=${firstRun.code}, timedOut=${firstRun.timedOut}`, "invariant", "simulated"));

  const serviceUrl = ctx.env.ZESTROKU_MISSION_SERVICE_URL;
  if (!serviceUrl) {
    cases.push(result("resident.http-health", "UNKNOWN", "ZESTROKU_MISSION_SERVICE_URL is not configured; live service health was not observed", "live-witness", "live"));
    cases.push(result("resident.http-inventory", "UNKNOWN", "ZESTROKU_MISSION_SERVICE_URL is not configured; live inventory was not observed", "live-witness", "live"));
  } else {
    try {
      const health = await fetchJson(new URL("/health", checkedServiceUrl(serviceUrl)).toString());
      const healthBody = health.body;
      const healthOk = health.status === 200 && typeof healthBody === "object" && healthBody !== null && "status" in healthBody && healthBody.status === "ok";
      cases.push(result("resident.http-health", healthOk ? "PASS" : "FAIL", healthOk ? "configured resident service returned HTTP 200 health status ok" : `health contract failed with HTTP ${health.status}`, "live-witness", "live"));
    } catch {
      cases.push(result("resident.http-health", "FAIL", "configured service health request failed or URL was invalid", "live-witness", "live"));
    }
    try {
      const inventory = await fetchJson(new URL("/v1/devices", checkedServiceUrl(serviceUrl)).toString());
      let rows: unknown;
      if (Array.isArray(inventory.body)) rows = inventory.body;
      else if (inventory.body && typeof inventory.body === "object" && "devices" in inventory.body) rows = inventory.body.devices;
      const count = Array.isArray(rows) ? rows.length : 0;
      const inventoryOk = inventory.status === 200 && count > 0;
      cases.push(result("resident.http-inventory", inventoryOk ? "PASS" : "FAIL", inventoryOk ? `configured resident service returned ${count} inventory rows` : `inventory contract requires HTTP 200 and at least one device row; got HTTP ${inventory.status}`, "live-witness", "live"));
    } catch {
      cases.push(result("resident.http-inventory", "FAIL", "configured service inventory request failed or URL was invalid", "live-witness", "live"));
    }
  }
  cases.push(await bindNegative(ctx));
  return cases;
}
