// scripts/mission/safe-control.ts
//
// Mission pillar check: safe-control (bead zestroku-zestroku-doctrine-a0h.14.3).
//
// WHAT: behavioral verdicts for the Keys/control mission row — approved
// mutations reach the device exactly once with a completed audit, while
// missing/blank/free-text/wrong/expired/replayed tokens, tier refusals and
// unknown principals cause zero mutation with an audited denial. Never
// approval-only success.
//
// HOW (all real behavior, no presence/load proxies):
// - Local: the real product binary against an isolated store (ZESTROKU_DB
//   under ctx.scratch) and a real loopback listener that counts POSTs.
//   Unconfirmed `key` must exit 4 with CONFIRMATION_REQUIRED and zero POSTs.
// - Remote: existing source-bound RCH suites on the immutable ctx.source
//   revision (RCH_REQUIRE_REMOTE=1, never a local build): replay single-use,
//   free-text denial, snapshot before-state audit, closed request bodies
//   (unknown principal can never ride the body), dispatch failures recorded
//   as failures. Only observed test names are referenced.
// - Boundary: `approve` must refuse outside an interactive owner terminal
//   without minting; live device reads are read-only `info` and only when
//   ZESTROKU_MISSION_LIVE_DEVICE is explicitly configured. This checker
//   issues no key/launch POST to any device or service.
//
// This module never mints approvals, never touches production tokens, and
// never targets a non-loopback device.
//
// Regex surface (deliberately minimal; literals use includes() per the
// weakest-engine rule): COMPLETED_RE (anchored dispatch-line grammar),
// REDACT_TOKEN (confirm value), REDACT_HEX (long hex runs). All regex inputs
// are capped at MAX_SCAN first; subprocess outputs are inherently bounded by
// the exec layer. Timed near-miss evidence lives in safe-control.test.ts.

import type { CheckCase, CheckContext } from "../mission-check";
import { createServer } from "node:http";
import type { Server } from "node:http";

const LOCAL_TIMEOUT_MS = 30_000;
const RCH_TIMEOUT_MS = 1_800_000;
// Upper bound for any string handed to a RegExp. Real verdict lines are
// under 200 chars; the cap only binds adversarial or runaway outputs.
const MAX_SCAN = 64_000;

const COMPLETED_RE = /^ok: (key|launch) \S+ \(audit id \d+\)\s*$/;
const REDACT_TOKEN = /confirm\s*[:=]\s*\S+/gi;
const REDACT_HEX = /\b[0-9a-f]{24,}\b/gi;

// Observed test entry points. Never invent a test name: each string below is
// a real #[test] fn in the named suite file.
const RCH_A_TARGET = ["-p", "zestroku-cli", "--test", "mutation_cli"];
const RCH_A_FILTERS = [
  "minted_cli_token_authorizes_once_then_replay_is_denied",
  "free_text_confirm_is_rejected_and_audited_without_dispatch",
  "unconfirmed_keypress_refuses_with_audit_exit4",
];
const RCH_B_TARGETS = [
  "-p", "zestroku-api",
  "--test", "mutation_gate_contract",
  "--test", "mutation_schema_contract",
  "--test", "mutation_outcome_contract",
];
const RCH_B_FILTERS = [
  "tier_refusal_precedes_http_token_consumption_and_snapshot_is_audited",
  "key_and_launch_mutation_routes_document_closed_request_bodies",
  "approved_http_dispatch_failures_finish_as_failures",
];

function result(
  id: string,
  outcome: CheckCase["outcome"],
  detail: string,
  claim: CheckCase["claim"] = "invariant",
  tier: CheckCase["tier"] = "simulated",
): CheckCase {
  return { id, outcome, detail, claim, tier };
}

/** Strip control whitespace and redact anything token-shaped before it can
 * reach diagnostics. Short (7-char) source revs pass through; long hex runs
 * and confirm values do not. Input capped before matching. */
export function sanitizeDetail(value: string): string {
  const capped = value.length > MAX_SCAN ? value.slice(0, MAX_SCAN) : value;
  return capped
    .replace(/[\r\n\t]+/g, " ")
    .replace(REDACT_TOKEN, "confirm=<redacted>")
    .replace(REDACT_HEX, "<hex-redacted>")
    .slice(0, 400);
}

/** True only for a real completed-dispatch line as printed by the CLI after
 * finish_mutation (mutate.rs: `ok: {kind} {target} (audit id {id})`).
 * Approval-only stub text ("dispatch happens in serve"), refusals, and empty
 * output are all false. Input capped before matching. */
export function isCompletedDispatchText(text: string): boolean {
  const capped = text.length > MAX_SCAN ? text.slice(0, MAX_SCAN) : text;
  return !capped.toLowerCase().includes("dispatch happens in serve") && COMPLETED_RE.test(capped.trim());
}

/** Token shape: 64 hex chars as minted by issue_approval (hex of 32 bytes).
 * Manual scan, no regex: called on approve stdout where a token must never
 * appear. */
function hasTokenShapedHex(text: string): boolean {
  const capped = text.length > MAX_SCAN ? text.slice(0, MAX_SCAN) : text;
  let run = 0;
  for (const ch of capped) {
    const hex =
      (ch >= "0" && ch <= "9") || (ch >= "a" && ch <= "f") || (ch >= "A" && ch <= "F");
    run = hex ? run + 1 : 0;
    if (run >= 64) return true;
  }
  return false;
}

function isFullRevision(value: string): boolean {
  const trimmed = value.trim();
  if (trimmed.length < 40 || trimmed.length > 64) return false;
  for (const ch of trimmed) {
    const hex =
      (ch >= "0" && ch <= "9") || (ch >= "a" && ch <= "f") || (ch >= "A" && ch <= "F");
    if (!hex) return false;
  }
  return true;
}

function cleanEnv(env: Record<string, string | undefined>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(env)) {
    if (value !== undefined) out[key] = value;
  }
  return out;
}

type ExecResult = { code: number; stdout: string; stderr: string; timedOut: boolean };

async function tryExec(
  ctx: CheckContext,
  argv: string[],
  env: Record<string, string | undefined>,
  timeoutMs: number,
): Promise<{ ok: true; value: ExecResult } | { ok: false; reason: string }> {
  try {
    const value = await ctx.exec(argv, { env: cleanEnv({ ...ctx.env, ...env }), timeoutMs });
    return { ok: true, value };
  } catch (error) {
    return { ok: false, reason: sanitizeDetail(String(error)) };
  }
}

async function denialZeroPost(ctx: CheckContext, versionOk: boolean): Promise<CheckCase> {
  const id = "safe-control.unconfirmed-denial-zero-post";
  if (!versionOk) {
    return result(id, "UNKNOWN", "product binary did not answer --version; denial behavior unobserved", "invariant", "simulated");
  }
  let postCount = 0;
  const server: Server = createServer((_req, res) => {
    if (_req.method === "POST") postCount += 1;
    res.statusCode = 500;
    res.end("fixture refuses everything");
  });
  try {
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", () => resolve());
    });
    const address = server.address();
    if (!address || typeof address === "string") {
      return result(id, "FAIL", "could not bind the loopback POST-count listener", "invariant", "simulated");
    }
    const scratch = `${ctx.scratch}/safe-control`;
    const attempted = await tryExec(
      ctx,
      [ctx.binary, "key", `127.0.0.1:${address.port}`, "HOME"],
      {
        ZESTROKU_DB: `${scratch}/denial.db`,
        HOME: `${scratch}/home`,
        ZESTROKU_TIER: "permissive",
      },
      LOCAL_TIMEOUT_MS,
    );
    if (!attempted.ok) {
      return result(id, "FAIL", `denial run could not execute: ${attempted.reason}`, "invariant", "simulated");
    }
    const { code, stdout, stderr, timedOut } = attempted.value;
    const output = `${stdout}\n${stderr}`;
    const denied = !timedOut && code === 4 && output.includes("CONFIRMATION_REQUIRED");
    const completed = isCompletedDispatchText(output);
    if (denied && !completed && postCount === 0) {
      return result(id, "PASS", "real unconfirmed keypress exited 4 CONFIRMATION_REQUIRED with zero POSTs to the loopback device", "invariant", "simulated");
    }
    return result(
      id,
      "FAIL",
      `expected exit 4 CONFIRMATION_REQUIRED with zero POSTs and no dispatch text; code=${code}, timedOut=${timedOut}, posts=${postCount}, completed=${completed}, output=${sanitizeDetail(output)}`,
      "invariant",
      "simulated",
    );
  } catch (error) {
    return result(id, "FAIL", `denial run could not execute: ${sanitizeDetail(String(error))}`, "invariant", "simulated");
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

async function rchSuite(
  ctx: CheckContext,
  target: string[],
  filters: string[],
): Promise<{ ran: boolean; unknownReason?: string; output?: string }> {
  const source = ctx.source.trim();
  if (!isFullRevision(source)) {
    return { ran: false, unknownReason: "ctx.source is not a full revision; immutable remote proof unaddressable" };
  }
  if (ctx.env.ZESTROKU_MISSION_ALLOW_RCH !== "1") {
    return { ran: false, unknownReason: "ZESTROKU_MISSION_ALLOW_RCH is not 1; remote suites run only in dispatcher runs with admitted slots" };
  }
  const attempted = await tryExec(
    ctx,
    ["rch", "exec", "--base", source, "--clean-overlay", "--",
      "cargo", "test", "-j", "2", "--locked", ...target, "--", ...filters],
    { RCH_REQUIRE_REMOTE: "1" },
    RCH_TIMEOUT_MS,
  );
  if (!attempted.ok) {
    return { ran: false, unknownReason: `rch invocation failed to start (slots or binary): ${attempted.reason}` };
  }
  const { code, stdout, stderr, timedOut } = attempted.value;
  if (timedOut || code !== 0) {
    return { ran: true, output: `REMOTE-FAILED code=${code} timedOut=${timedOut} ${sanitizeDetail(`${stdout}\n${stderr}`)}` };
  }
  return { ran: true, output: `${stdout}\n${stderr}` };
}

function suiteCase(
  id: string,
  suite: { ran: boolean; unknownReason?: string; output?: string },
  names: string[],
  passDetail: string,
): CheckCase {
  if (!suite.ran) {
    return result(id, "UNKNOWN", suite.unknownReason ?? "remote suite did not run; behavior unobserved", "invariant", "simulated");
  }
  const output = suite.output ?? "";
  if (output.startsWith("REMOTE-FAILED")) {
    return result(id, "FAIL", `remote suite failed: ${sanitizeDetail(output)}`, "invariant", "simulated");
  }
  const missing = names.filter((name) => !output.includes(`test ${name} ... ok`));
  const suiteOk = output.includes("test result: ok");
  if (missing.length === 0 && suiteOk) {
    return result(id, "PASS", passDetail, "invariant", "simulated");
  }
  return result(id, "FAIL", `expected green runs of ${names.join(", ")}; missing=[${missing.join(", ")}], suiteOk=${suiteOk}`, "invariant", "simulated");
}

async function approvalBoundary(ctx: CheckContext, versionOk: boolean): Promise<CheckCase> {
  const id = "safe-control.live-approval-boundary";
  if (!versionOk) {
    return result(id, "UNKNOWN", "product binary did not answer --version; boundary behavior unobserved", "invariant", "simulated");
  }
  const attempted = await tryExec(
    ctx,
    [ctx.binary, "approve", "key", "127.0.0.1:9", "HOME"],
    { HOME: `${ctx.scratch}/safe-control/home` },
    LOCAL_TIMEOUT_MS,
  );
  if (!attempted.ok) {
    return result(id, "FAIL", `boundary probe could not execute: ${attempted.reason}`, "invariant", "simulated");
  }
  const { code, stdout, stderr, timedOut } = attempted.value;
  const output = `${stdout}\n${stderr}`;
  const lowered = output.toLowerCase();
  const refused =
    !timedOut && code !== 0 &&
    (lowered.includes("requires an interactive terminal") || lowered.includes("did not match"));
  const minted = hasTokenShapedHex(stdout);
  if (refused && !minted) {
    return result(id, "PASS", `approve refused outside an owner terminal (code=${code}) and emitted no token`, "invariant", "simulated");
  }
  return result(id, "FAIL", `approve must refuse non-interactive minting without emitting a token; code=${code}, timedOut=${timedOut}, minted=${minted}, output=${sanitizeDetail(output)}`, "invariant", "simulated");
}

async function liveDeviceReadOnly(ctx: CheckContext, versionOk: boolean): Promise<CheckCase> {
  const id = "safe-control.live-device-read-only";
  const device = (ctx.env.ZESTROKU_MISSION_LIVE_DEVICE ?? "").trim();
  if (!device) {
    return result(id, "UNKNOWN", "ZESTROKU_MISSION_LIVE_DEVICE is not configured; no physical device was touched", "live-witness", "live");
  }
  if (!versionOk) {
    return result(id, "UNKNOWN", "product binary did not answer --version; live read unobserved", "live-witness", "live");
  }
  const attempted = await tryExec(
    ctx,
    [ctx.binary, "info", device],
    {
      ZESTROKU_DB: `${ctx.scratch}/safe-control/live-read.db`,
      HOME: `${ctx.scratch}/safe-control/home`,
    },
    LOCAL_TIMEOUT_MS,
  );
  if (!attempted.ok) {
    return result(id, "FAIL", `live read could not execute: ${attempted.reason}`, "live-witness", "live");
  }
  const { code, stdout, stderr, timedOut } = attempted.value;
  if (!timedOut && code === 0) {
    return result(id, "PASS", `read-only info against the configured live device succeeded; no mutation was attempted (${sanitizeDetail(`${stdout} ${stderr}`)})`, "live-witness", "live");
  }
  return result(id, "FAIL", `live read-only info failed; code=${code}, timedOut=${timedOut}, output=${sanitizeDetail(`${stdout}\n${stderr}`)}`, "live-witness", "live");
}

async function undoSurface(ctx: CheckContext, versionOk: boolean): Promise<CheckCase> {
  const id = "safe-control.undo-surface";
  if (!versionOk) {
    return result(id, "UNKNOWN", "product binary did not answer --version; undo surface unobserved", "invariant", "simulated");
  }
  const attempted = await tryExec(ctx, [ctx.binary, "undo", "--help"], {}, LOCAL_TIMEOUT_MS);
  if (!attempted.ok) {
    return result(id, "FAIL", `undo probe could not execute: ${attempted.reason}`, "invariant", "simulated");
  }
  if (attempted.value.code === 0) {
    return result(id, "FAIL", "an undo entry point answered successfully; this checker has no undo consumer to exercise it", "invariant", "simulated");
  }
  return result(id, "UNKNOWN", `no undo entry point (code=${attempted.value.code}); every approval must be treated as final, no undo verdict expressible`, "invariant", "simulated");
}

export async function run(ctx: CheckContext): Promise<CheckCase[]> {
  const cases: CheckCase[] = [];

  const versionAttempt = await tryExec(ctx, [ctx.binary, "--version"], {}, LOCAL_TIMEOUT_MS);
  const versionOk = versionAttempt.ok && !versionAttempt.value.timedOut && versionAttempt.value.code === 0;
  const versionText = versionAttempt.ok
    ? `${versionAttempt.value.stdout} ${versionAttempt.value.stderr}`.trim()
    : "";
  const revision = isFullRevision(ctx.source) ? ctx.source.trim().slice(0, 7) : "";
  if (!versionOk) {
    const why = versionAttempt.ok
      ? `code=${versionAttempt.value.code}, timedOut=${versionAttempt.value.timedOut}`
      : `spawn failed: ${versionAttempt.reason}`;
    cases.push(result("safe-control.source-bound-artifact", "UNKNOWN", `binary did not answer --version (${why}); source binding unobserved`, "invariant", "fixture"));
  } else if (revision === "") {
    cases.push(result("safe-control.source-bound-artifact", "UNKNOWN", "ctx.source is not a full revision; version binding uncheckable", "invariant", "fixture"));
  } else {
    const bound = versionText.includes(revision) && !versionText.includes("-dirty");
    cases.push(result(
      "safe-control.source-bound-artifact",
      bound ? "PASS" : "FAIL",
      bound
        ? `binary version identifies source ${revision} with no dirty suffix`
        : `binary version does not identify source ${revision} or is dirty: ${sanitizeDetail(versionText)}`,
      "invariant",
      "fixture",
    ));
  }

  cases.push(await denialZeroPost(ctx, versionOk));

  const rchA = await rchSuite(ctx, RCH_A_TARGET, RCH_A_FILTERS);
  cases.push(suiteCase(
    "safe-control.replay-single-use",
    rchA,
    ["minted_cli_token_authorizes_once_then_replay_is_denied"],
    "minted CLI token authorized exactly once; replay denied with audit (remote mutation_cli)",
  ));
  cases.push(suiteCase(
    "safe-control.free-text-denial-no-dispatch",
    rchA,
    ["free_text_confirm_is_rejected_and_audited_without_dispatch", "unconfirmed_keypress_refuses_with_audit_exit4"],
    "free-text and absent confirm refused and audited without dispatch (remote mutation_cli)",
  ));

  const rchB = await rchSuite(ctx, RCH_B_TARGETS, RCH_B_FILTERS);
  cases.push(suiteCase(
    "safe-control.snapshot-before-state-audited",
    rchB,
    ["tier_refusal_precedes_http_token_consumption_and_snapshot_is_audited"],
    "tier refusal preceded token consumption with the active-app snapshot audited (remote mutation_gate_contract)",
  ));
  cases.push(suiteCase(
    "safe-control.unknown-principal-closed-body",
    rchB,
    ["key_and_launch_mutation_routes_document_closed_request_bodies"],
    "mutation bodies stay closed: host/port/tier/principal/backend_url cannot ride the request (remote mutation_schema_contract)",
  ));
  cases.push(suiteCase(
    "safe-control.dispatch-failure-recorded-as-failure",
    rchB,
    ["approved_http_dispatch_failures_finish_as_failures"],
    "approved dispatch failures finished as failures, never success (remote mutation_outcome_contract)",
  ));

  cases.push(await approvalBoundary(ctx, versionOk));
  cases.push(await liveDeviceReadOnly(ctx, versionOk));
  cases.push(await undoSurface(ctx, versionOk));

  return cases;
}
