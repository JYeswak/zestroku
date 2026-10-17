// scripts/mission/safe-control.ts
//
// Mission pillar check: safe-control (bead zestroku-zestroku-doctrine-a0h.14.3).
//
// WHAT: behavioral verdicts for the Keys/control mission row — approved
// mutations reach the device exactly once with a completed audit, while
// missing/blank/free-text/wrong/expired/replayed tokens, tier refusals and
// closed-body violations cause zero mutation with an audited denial. Never
// approval-only success.
//
// EVIDENCE TIERS (never confused):
// - Local real behavior (simulated): the real product binary against an
//   isolated store (ZESTROKU_DB under ctx.scratch) and a real loopback
//   listener that counts POSTs. Exit codes and socket counts are observed;
//   audit-row persistence for the local run is NOT claimed (no CLI read
//   surface exists for the actions table) — row logging is proven by the
//   Rust suites below, which read the store directly.
// - Rust suites (simulated): RCH uses immutable ctx.source when selected;
//   local cargo is the fallback when RCH is absent. Local results do not
//   establish immutable-source or product-build provenance. Only observed
//   #[test] names must pass; unrelated selected test binaries may report zero
//   matches, but a missing expected name remains an explicit FAIL.
// - Live (live): read-only `info` only when ZESTROKU_MISSION_LIVE_DEVICE is
//   configured; absent live prerequisites are UNKNOWN, never PASS.
// - stdout/stderr text alone never proves audit completion: mutate.rs warns
//   on finish_mutation failure yet still prints `ok:` and exits 0, and the
//   MCP stub prints approval-only success without dispatch. Completed-audit
//   claims rest on store-reading suites (mutation_api), never on text.
//
// COVERS: each case lists only obligation polarities it fully asserts, from
// the safe-control set (cap.key/launch/install/history/undo/policy ×
// positive/negative). The closed-schema case proves request-body smuggling
// is refused — it does NOT prove unknown-principal owner mapping (a tailnet
// surface outside this pillar); that limitation is stated in its detail.
// cap.undo rows are intentionally left for auto-coverage UNKNOWN: the
// product exposes no undo acceptance to consume.
//
// ENV NAMES (all inputs bound here, no hidden transitive sources):
// ZESTROKU_MISSION_RUN_RCH=1 when RCH is selected and 0 for local cargo,
// ZESTROKU_MISSION_LIVE_DEVICE (owner device host[:port] for read-only
// info), ZESTROKU_DB / ZESTROKU_TIER / HOME (isolated per-run store, tier
// override, home dir under ctx.scratch). The dispatcher sets
// RCH_REQUIRE_REMOTE=1 for every remote invocation. Transitive sources: ctx.source
// (immutable base), ctx.binary (dispatcher-supplied), ctx.scratch.
//
// This module never mints approvals, never touches production tokens, never
// targets a non-loopback device, and issues no key/launch POST anywhere.
//
// Regex surface (deliberately minimal; literals use includes()): COMPLETED_RE
// (anchored dispatch-line grammar), REDACT_TOKEN, REDACT_HEX. All regex
// inputs are capped at MAX_SCAN first. Timed near-miss evidence lives in
// safe-control.test.ts.

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
  "sideload_requires_confirmation_then_authenticates_and_audits_on_installer_port",
];
const RCH_B_TARGETS = [
  "-p", "zestroku-api",
  "--test", "mutation_gate_contract",
  "--test", "mutation_schema_contract",
  "--test", "mutation_outcome_contract",
];
const RCH_B_FILTERS = [
  "tier_refusal_precedes_http_token_consumption_and_snapshot_is_audited",
  "http_approval_requires_nonblank_single_use_device_and_command_binding",
  "open_and_unknown_http_tiers_deny_before_mutation_post",
  "key_and_launch_mutation_routes_document_closed_request_bodies",
  "approved_http_dispatch_failures_finish_as_failures",
];
const RCH_C_TARGET = ["-p", "zestroku-cli", "--test", "mutation_api"];
const RCH_C_FILTERS = ["real_http_mutations_use_inventory_bound_approval_snapshot_and_audit"];

function result(
  id: string,
  outcome: CheckCase["outcome"],
  detail: string,
  claim: CheckCase["claim"] = "invariant",
  tier: CheckCase["tier"] = "simulated",
  covers?: readonly string[],
): CheckCase {
  return covers === undefined ? { id, outcome, detail, claim, tier } : { id, outcome, detail, claim, tier, covers };
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
 * dispatch (mutate.rs: `ok: {kind} {target} (audit id {id})`). Approval-only
 * stub text ("dispatch happens in serve"), refusals, and empty output are all
 * false. A true return is a dispatch signal, NOT audit-completion proof:
 * finish_mutation failure only warns while `ok:` still prints. */
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
      return result(
        id, "PASS",
        "real unconfirmed keypress exited 4 CONFIRMATION_REQUIRED with zero POSTs to the loopback device; audit-row persistence for this local run is unverified (no CLI read surface for the actions table) and rests on the RCH row-reading suites",
        "invariant", "simulated", ["cap.key.negative"],
      );
    }
    return result(
      id,
      "FAIL",
      `expected exit 4 CONFIRMATION_REQUIRED with zero POSTs and no dispatch text; code=${code}, timedOut=${timedOut}, posts=${postCount}, completed=${completed}, output=${sanitizeDetail(output)}`,
      "invariant", "simulated",
    );
  } catch (error) {
    return result(id, "FAIL", `denial run could not execute: ${sanitizeDetail(String(error))}`, "invariant", "simulated");
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

async function rustTestSuite(
  ctx: CheckContext,
  target: string[],
  filters: string[],
): Promise<{ ran: boolean; unknownReason?: string; output?: string }> {
  const source = ctx.source.trim();
  if (!isFullRevision(source)) {
    return { ran: false, unknownReason: "ctx.source is not a full revision; Rust suite source is unaddressable" };
  }
  const executor = ctx.env.ZESTROKU_MISSION_RUN_RCH;
  if (executor !== "1" && executor !== "0") {
    return { ran: false, unknownReason: "Rust test executor was not selected" };
  }
  const runner = executor === "1" ? "remote RCH" : "local cargo";
  try {
    const command = await ctx.runCargoTest(
      ["test", "-j", "2", "--locked", ...target, "--", ...filters],
      { timeoutMs: RCH_TIMEOUT_MS },
    );
    if (command.stderr === "EXECUTABLE_UNAVAILABLE") {
      return { ran: false, unknownReason: `${runner} executor is unavailable` };
    }
    if (command.timedOut || command.code !== 0) {
      return { ran: true, output: `SUITE-FAILED ${runner} code=${command.code} timedOut=${command.timedOut} ${sanitizeDetail(`${command.stdout}\n${command.stderr}`)}` };
    }
    return { ran: true, output: `${command.stdout}\n${command.stderr}` };
  } catch {
    return { ran: false, unknownReason: `${runner} cargo test command could not start` };
  }
}

function suiteCase(
  id: string,
  suite: { ran: boolean; unknownReason?: string; output?: string },
  names: string[],
  passDetail: string,
  covers?: readonly string[],
): CheckCase {
  if (!suite.ran) {
    return result(id, "UNKNOWN", suite.unknownReason ?? "Rust test suite did not run; behavior unobserved", "invariant", "simulated");
  }
  const output = suite.output ?? "";
  if (output.startsWith("SUITE-FAILED")) {
    return result(id, "FAIL", `Rust suite failed: ${sanitizeDetail(output)}`, "invariant", "simulated");
  }
  const missing = names.filter((name) => !output.includes(`test ${name} ... ok`));
  const suiteOk = output.includes("test result: ok");
  // Cargo emits one result per selected test target; a zero-run sibling is
  // harmless only when every requested test passes in a green suite.
  if (missing.length === 0 && suiteOk) {
    return result(id, "PASS", passDetail, "invariant", "simulated", covers);
  }
  if (missing.length > 0 && output.includes("test result: ok. 0 passed")) {
    return result(id, "FAIL", `expected named tests missing in a zero-run target; expected ${names.join(", ")}, missing=[${missing.join(", ")}]`, "invariant", "simulated");
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

/** Approval finality: the applicable consumer of the undo probe. Absence of
 * any inverse entry point is observed support for "every approval is final".
 * A present inverse is reported as unexercised (UNKNOWN), never called
 * broken: this checker has no isolated undo consumer for an unknown future
 * CLI shape, and inventing one would be fake coverage. */
async function finalityNoInverse(ctx: CheckContext, versionOk: boolean): Promise<CheckCase> {
  const id = "safe-control.approval-finality-no-inverse";
  if (!versionOk) {
    return result(id, "UNKNOWN", "product binary did not answer --version; inverse surface unobserved", "invariant", "simulated");
  }
  const attempted = await tryExec(ctx, [ctx.binary, "undo", "--help"], {}, LOCAL_TIMEOUT_MS);
  if (!attempted.ok) {
    return result(id, "FAIL", `inverse probe could not execute: ${attempted.reason}`, "invariant", "simulated");
  }
  if (attempted.value.code === 0) {
    return result(id, "UNKNOWN", "an inverse entry point answered; undo semantics are unexercised by this checker (no isolated undo consumer), so no undo verdict is expressed", "invariant", "simulated");
  }
  return result(id, "PASS", `no inverse entry point (code=${attempted.value.code}); approvals are final, undo acceptance is absent rather than assumed`, "invariant", "simulated");
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

  const rev = revision === "" ? ctx.source.trim().slice(0, 12) : revision;
  const execution = ctx.env.ZESTROKU_MISSION_RUN_RCH === "1" ? "remote RCH" : "local cargo";
  const rchA = await rustTestSuite(ctx, RCH_A_TARGET, RCH_A_FILTERS);
  cases.push(suiteCase(
    "safe-control.replay-single-use",
    rchA,
    ["minted_cli_token_authorizes_once_then_replay_is_denied"],
    `minted CLI key token authorized once then replay denied with denied audit row (${execution} mutation_cli @${rev})`,
    ["cap.key.negative", "cap.history.negative"],
  ));
  cases.push(suiteCase(
    "safe-control.free-text-denial-no-dispatch",
    rchA,
    ["free_text_confirm_is_rejected_and_audited_without_dispatch", "unconfirmed_keypress_refuses_with_audit_exit4"],
    `free-text and absent key confirm refused and audited without dispatch (${execution} mutation_cli @${rev})`,
    ["cap.key.negative", "cap.history.negative"],
  ));
  cases.push(suiteCase(
    "safe-control.install-gated-confirmation-audit",
    rchA,
    ["sideload_requires_confirmation_then_authenticates_and_audits_on_installer_port"],
    `unconfirmed sideload refused before installer HTTP; confirmed install authenticated and audited (${execution} mutation_cli @${rev})`,
    ["cap.install.negative"],
  ));

  const rchB = await rustTestSuite(ctx, RCH_B_TARGETS, RCH_B_FILTERS);
  cases.push(suiteCase(
    "safe-control.snapshot-before-state-audited",
    rchB,
    ["tier_refusal_precedes_http_token_consumption_and_snapshot_is_audited"],
    `launch tier refusal preceded token consumption with zero POSTs and the active-app snapshot audited (${execution} mutation_gate_contract @${rev})`,
    ["cap.launch.negative", "cap.policy.negative"],
  ));
  cases.push(suiteCase(
    "safe-control.invalid-token-binding-denied-audited",
    rchB,
    ["http_approval_requires_nonblank_single_use_device_and_command_binding", "open_and_unknown_http_tiers_deny_before_mutation_post"],
    `missing/blank/free-text/wrong-action/wrong-device/expired tokens and open/unknown tiers denied with audited rows and snapshot-only device traffic (${execution} mutation_gate_contract @${rev})`,
    ["cap.launch.negative", "cap.history.negative", "cap.policy.negative"],
  ));
  cases.push(suiteCase(
    "safe-control.body-smuggling-refused-closed-schema",
    rchB,
    ["key_and_launch_mutation_routes_document_closed_request_bodies"],
    `mutation request bodies stay closed: host/port/tier/principal/backend_url cannot ride the body (${execution} mutation_schema_contract @${rev}); unknown-principal owner mapping is a tailnet surface outside this pillar and remains unproven here`,
    ["cap.policy.negative"],
  ));
  cases.push(suiteCase(
    "safe-control.dispatch-failure-recorded-as-failure",
    rchB,
    ["approved_http_dispatch_failures_finish_as_failures"],
    `approved launch dispatch failures finished as failures, never completed, after snapshot plus exactly one POST (${execution} mutation_outcome_contract @${rev})`,
    ["cap.launch.negative", "cap.history.positive"],
  ));

  const rchC = await rustTestSuite(ctx, RCH_C_TARGET, RCH_C_FILTERS);
  cases.push(suiteCase(
    "safe-control.completed-dispatch-with-true-before-state",
    rchC,
    ["real_http_mutations_use_inventory_bound_approval_snapshot_and_audit"],
    `real daemon plus SimDevice: approved launch then key each dispatched exactly once with completed audit rows carrying true before-states (tier_source=recorded snapshots), failure recorded as failure, tokens absent from every surface (${execution} mutation_api @${rev})`,
    ["cap.key.positive", "cap.launch.positive", "cap.history.positive"],
  ));

  cases.push(await approvalBoundary(ctx, versionOk));
  cases.push(await liveDeviceReadOnly(ctx, versionOk));
  cases.push(await finalityNoInverse(ctx, versionOk));

  return cases;
}
