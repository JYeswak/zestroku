// scripts/mission/safe-control.test.ts
//
// Planted-negative and positive-control tests for safe-control.ts.
//
// Honesty contract (chief M5, bead .14.3 [a]; mails 54047/54053/54055):
// - Fixtures below implement only the OBSERVABLE contract (exit codes,
//   output shapes, real loopback HTTP exchanges). They never stand in for
//   product proof: green-against-fixture shows the checker logic CAN pass,
//   red-against-planted shows it CAN fail. Real product proof comes from
//   dispatcher runs against the real binary and RCH suites.
// - No mocked command echoes as success evidence (no stubbed RCH output is
//   ever asserted PASS), no source/file presence checks, no failure-forcing
//   env flag, no regex on product source.
// - Real sockets (loopback only), real subprocesses, real policy strings
//   copied verbatim from product source (mutate.rs, mcp/lib.rs).
// - Never mints approvals, never touches a non-loopback host, never writes
//   outside the test scratch dir (left for the scratch reaper, never rm -rf).
// - Full-suite-safe: no local Darwin artifact, RCH, or live credentials
//   required; remote/live legs stay UNKNOWN without opt-in env.

import { describe, test, expect, beforeAll } from "bun:test";
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createServer } from "node:http";
import type { Server } from "node:http";
import { run, isCompletedDispatchText, sanitizeDetail } from "./safe-control";
import type { CheckCase, CheckContext } from "../mission-check";

const REPO = resolve(import.meta.dir, "..", "..");
// Platform temp dir (os.tmpdir() honors TMPDIR): runs on a stock runner with
// the default TMPDIR; fleet agents export their owned var/agent-tmp TMPDIR.
const SCRATCH = join(tmpdir(), `safe-control-test.${process.pid}`);
const LOCAL_TIMEOUT = 30_000;
// Full-revision-shaped test input (not a real commit).
const TEST_REV = "f".repeat(40);
const TEST_SHORT = TEST_REV.slice(0, 7);

// The dispatcher's registered safe-control obligations; every covers entry
// emitted by run() must fall in this set or integration fails the pillar
// with coverage:unowned-obligation.
const ALLOWED_COVERS = [
  "cap.key", "cap.launch", "cap.install", "cap.history", "cap.undo", "cap.policy",
].flatMap((id) => [`${id}.positive`, `${id}.negative`]);

type ExecResult = { code: number; stdout: string; stderr: string; timedOut: boolean };
type ExecFn = (argv: string[], options?: { env?: Record<string, string | undefined>; timeoutMs?: number }) => Promise<ExecResult>;

function realExec(
  argv: string[],
  options?: { env?: Record<string, string | undefined>; timeoutMs?: number },
): Promise<ExecResult> {
  // Exception: this is a kill-switch for real subprocesses, not a test
  // sleep; the timeout never adds latency to a passing run.
  const timeoutMs = options?.timeoutMs ?? LOCAL_TIMEOUT;
  const { promise, resolve } = Promise.withResolvers<ExecResult>();
  const child = spawn(argv[0], argv.slice(1), {
    env: { ...process.env, ...options?.env } as Record<string, string>,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    child.kill("SIGKILL");
  }, timeoutMs);
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
    if (stdout.length > 262_144) stdout = stdout.slice(-262_144);
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
    if (stderr.length > 262_144) stderr = stderr.slice(-262_144);
  });
  child.on("error", (error) => {
    clearTimeout(timer);
    resolve({ code: 127, stdout: "", stderr: String(error), timedOut: false });
  });
  child.on("close", (code) => {
    clearTimeout(timer);
    resolve({ code: code ?? 1, stdout, stderr, timedOut });
  });
  return promise;
}

function ctxFor(binary: string, env: Record<string, string | undefined>, scratch: string, exec?: ExecFn, runCargoTest?: ExecFn): CheckContext {
  return {
    repo: REPO,
    binary,
    source: TEST_REV,
    scratch,
    env,
    exec: exec ?? realExec,
    runCargoTest: runCargoTest ?? (async () => ({ code: 127, stdout: "", stderr: "RUST_EXECUTOR_UNSELECTED", timedOut: false })),
  };
}

function byId(cases: CheckCase[], id: string): CheckCase {
  const found = cases.find((c) => c.id === id);
  if (!found) throw new Error(`missing case ${id}`);
  return found;
}

function assertNoApprovalOnlyPass(cases: CheckCase[]): void {
  for (const c of cases) {
    if (c.outcome === "PASS" && c.detail.toLowerCase().includes("dispatch happens in serve")) {
      throw new Error(`approval-only text passed in ${c.id}`);
    }
  }
}

const RCH_CASE_IDS = [
  "safe-control.replay-single-use",
  "safe-control.free-text-denial-no-dispatch",
  "safe-control.install-gated-confirmation-audit",
  "safe-control.snapshot-before-state-audited",
  "safe-control.invalid-token-binding-denied-audited",
  "safe-control.body-smuggling-refused-closed-schema",
  "safe-control.dispatch-failure-recorded-as-failure",
  "safe-control.completed-dispatch-with-true-before-state",
];

// Conforming fixture: implements the required observable contract.
// --version binds the test revision; key-without-confirm refuses exit 4;
// approve refuses non-TTY without minting; no undo entry point.
const BIN_OK = `#!/bin/sh
if [ "$1" = "--version" ]; then echo "zestroku 0.1.0 ${TEST_SHORT}"; exit 0; fi
if [ "$1" = "approve" ]; then echo "zestroku approve requires an interactive terminal" >&2; exit 2; fi
if [ "$1" = "undo" ]; then echo "unknown command: undo" >&2; exit 2; fi
if [ "$1" = "key" ]; then echo "zestroku: CONFIRMATION_REQUIRED - pass --confirm <token> (audit id 7)" >&2; exit 4; fi
echo "unknown command: $1" >&2; exit 2
`;

// Planted fixture: approval-only success. --version is correct (so a
// blanket-red checker cannot hide behind the artifact case); key reports the
// exact real stub text from zestroku-mcp with exit 0 and dispatches nothing;
// approve mints outside a terminal; undo claims success without semantics.
const BIN_STUB = `#!/bin/sh
if [ "$1" = "--version" ]; then echo "zestroku 0.1.0 ${TEST_SHORT}"; exit 0; fi
if [ "$1" = "approve" ]; then echo "deadbeefcafe0001"; exit 0; fi
if [ "$1" = "undo" ]; then echo "undo ok"; exit 0; fi
if [ "$1" = "key" ]; then echo "approved: audit id 3; dispatch happens in serve (m7c)"; exit 0; fi
echo "unknown command: $1" >&2; exit 2
`;

let binOk = "";
let binStub = "";

beforeAll(() => {
  mkdirSync(join(SCRATCH, ".owner-dir"), { recursive: true });
  writeFileSync(
    join(SCRATCH, ".owner"),
    JSON.stringify({ pid: process.pid, label: "safe-control-test", repo: REPO }),
  );
  binOk = join(SCRATCH, "bin-ok");
  binStub = join(SCRATCH, "bin-stub");
  writeFileSync(binOk, BIN_OK, { mode: 0o755 });
  writeFileSync(binStub, BIN_STUB, { mode: 0o755 });
  chmodSync(binOk, 0o755);
  chmodSync(binStub, 0o755);
});

describe("dispatch-line classifier", () => {
  test("accepts the real CLI completed lines", () => {
    expect(isCompletedDispatchText("ok: key HOME (audit id 12)")).toBe(true);
    expect(isCompletedDispatchText("ok: launch 8080 (audit id 3)\n")).toBe(true);
  });

  test("rejects the real approval-only stub line", () => {
    expect(isCompletedDispatchText("approved: audit id 3; dispatch happens in serve (m7c)")).toBe(false);
  });

  test("rejects refusals, empty output, and trailing junk", () => {
    expect(isCompletedDispatchText("zestroku: CONFIRMATION_REQUIRED (audit id 7)")).toBe(false);
    expect(isCompletedDispatchText("")).toBe(false);
    expect(isCompletedDispatchText("ok: key HOME (audit id 12) and more")).toBe(false);
    expect(isCompletedDispatchText("ok: key HOME (audit id abc)")).toBe(false);
  });

  test("classifier is a dispatch signal, not audit proof", () => {
    // mutate.rs warns on finish_mutation failure yet still prints ok: and
    // exits 0, so a true return must never be read as a completed audit row.
    // This test pins the documented limitation next to the predicate.
    expect(isCompletedDispatchText("ok: key HOME (audit id 12)")).toBe(true);
  });

  test("regex surface stays linear on adversarial input", () => {
    // Exception: measures real matcher latency on adversarial input in the
    // shipping engine; no sleep, no race, asserts a bound instead of waiting.
    const nearMissDispatch = `ok: key ${"A".repeat(100_000)}`;
    const nearMissHex = "f".repeat(100_000);
    const nearMissConfirm = `confirm = ${"x".repeat(100_000)}`;
    const start = Date.now();
    expect(isCompletedDispatchText(nearMissDispatch)).toBe(false);
    expect(sanitizeDetail(nearMissHex).includes("f".repeat(24))).toBe(false);
    expect(sanitizeDetail(nearMissConfirm).includes("x".repeat(24))).toBe(false);
    expect(Date.now() - start).toBeLessThan(2_000);
  });

  test("sanitizer redacts token-shaped material and keeps short revs", () => {
    const token = "a".repeat(64);
    const detail = sanitizeDetail(`denied confirm=${token} at f333305\nsecond line`);
    expect(detail.includes(token)).toBe(false);
    expect(detail.includes("confirm=<redacted>")).toBe(true);
    expect(detail.includes("f333305")).toBe(true);
    expect(detail.includes("\n")).toBe(false);
  });
});

describe("loopback POST policy, both polarities", () => {
  async function probeOnce(handler: (posts: { count: number }) => Server): Promise<{ status: number; body: string; posts: number }> {
    const posts = { count: 0 };
    const server = handler(posts);
    const { promise, resolve, reject } = Promise.withResolvers<void>();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
    await promise;
    try {
      const address = server.address();
      if (!address || typeof address === "string") throw new Error("no loopback address");
      // Exception: bounded fetch against a real loopback server needs a
      // genuine deadline; fake timers cannot abort a real socket.
      const response = await fetch(`http://127.0.0.1:${address.port}/v1/devices/127.0.0.1:8060/key`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ key: "HOME", confirm: "free-text-token" }),
        signal: AbortSignal.timeout(5_000),
      });
      return { status: response.status, body: await response.text(), posts: posts.count };
    } finally {
      await new Promise<void>((done) => server.close(() => done()));
    }
  }

  test("conforming server denies the unauthenticated POST (positive control)", async () => {
    const seen = await probeOnce((posts) =>
      createServer((req, res) => {
        if (req.method === "POST") posts.count += 1;
        res.statusCode = 403;
        res.end(JSON.stringify({ error: "confirmation_required" }));
      }),
    );
    expect(seen.posts).toBe(1);
    expect(seen.status).toBe(403);
    expect(seen.body.includes("confirmation_required")).toBe(true);
  });

  test("planted server grants the unauthenticated POST (must be judged a violation)", async () => {
    const seen = await probeOnce((posts) =>
      createServer((req, res) => {
        if (req.method === "POST") posts.count += 1;
        res.statusCode = 200;
        res.end(JSON.stringify({ action_id: 1, outcome: "completed" }));
      }),
    );
    expect(seen.posts).toBe(1);
    expect(seen.status).toBe(200);
    // The checker predicate for an unauthenticated mutation POST is
    // status 403; a 200 here is the planted violation, never a pass.
    expect(seen.status === 403).toBe(false);
    expect(isCompletedDispatchText(seen.body)).toBe(false);
  });
});

describe("run() against executable fixtures", () => {
  test("planted approval-only binary goes red, artifact stays green", async () => {
    const cases = await run(ctxFor(binStub, {}, join(SCRATCH, "planted")));
    expect(byId(cases, "safe-control.source-bound-artifact").outcome).toBe("PASS");
    expect(byId(cases, "safe-control.unconfirmed-denial-zero-post").outcome).toBe("FAIL");
    expect(byId(cases, "safe-control.live-approval-boundary").outcome).toBe("FAIL");
    expect(byId(cases, "safe-control.approval-finality-no-inverse").outcome).toBe("UNKNOWN");
    assertNoApprovalOnlyPass(cases);
  }, LOCAL_TIMEOUT);

  test("conforming binary passes local cases, remotes stay UNKNOWN", async () => {
    const cases = await run(ctxFor(binOk, {}, join(SCRATCH, "conforming")));
    expect(byId(cases, "safe-control.source-bound-artifact").outcome).toBe("PASS");
    expect(byId(cases, "safe-control.unconfirmed-denial-zero-post").outcome).toBe("PASS");
    expect(byId(cases, "safe-control.live-approval-boundary").outcome).toBe("PASS");
    expect(byId(cases, "safe-control.approval-finality-no-inverse").outcome).toBe("PASS");
    for (const id of [...RCH_CASE_IDS, "safe-control.live-device-read-only"]) {
      expect(byId(cases, id).outcome).toBe("UNKNOWN");
    }
    expect(cases.some((c) => c.outcome === "FAIL")).toBe(false);
    assertNoApprovalOnlyPass(cases);
  }, LOCAL_TIMEOUT);

  test("covers stay inside the registered safe-control obligations", async () => {
    const cases = await run(ctxFor(binOk, {}, join(SCRATCH, "covers")));
    for (const c of cases) {
      const covers = c.covers ?? [];
      for (let i = 0; i < covers.length; i++) {
        expect(ALLOWED_COVERS.includes(covers[i])).toBe(true);
        expect(covers.indexOf(covers[i])).toBe(i);
      }
    }
    expect(byId(cases, "safe-control.unconfirmed-denial-zero-post").covers).toEqual(["cap.key.negative"]);
    expect(cases.some((c) => (c.covers ?? []).some((id) => id.startsWith("cap.undo")))).toBe(false);
  }, LOCAL_TIMEOUT);

  test("zero-run remote output is FAIL, not green", async () => {
    // Causal evaluator negative on the checker's own zero-run guard. The
    // crafted output is never asserted PASS anywhere in this file.
    const zeroRun: ExecFn = async () => ({
      code: 0,
      stdout: "test result: ok. 0 passed; 0 failed; 0 ignored;",
      stderr: "",
      timedOut: false,
    });
    const cases = await run(ctxFor(binOk, { ZESTROKU_MISSION_RUN_RCH: "1" }, join(SCRATCH, "zerorun"), realExec, zeroRun));
    const replay = byId(cases, "safe-control.replay-single-use");
    expect(replay.outcome).toBe("FAIL");
    expect(replay.detail.includes("zero-run")).toBe(true);
  }, LOCAL_TIMEOUT);

  test("an unrelated target zero-run does not hide a requested passing test", async () => {
    const output = [
      "running 1 test",
      "test tier_refusal_precedes_http_token_consumption_and_snapshot_is_audited ... ok",
      "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;",
      "running 1 test",
      "test mutation_routes_document_closed_request_bodies ... ok",
      "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;",
      "running 0 tests",
      "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;",
    ].join("\n");
    const mixedTargets: ExecFn = async () => ({
      code: 0,
      stdout: output,
      stderr: "",
      timedOut: false,
    });
    const cases = await run(ctxFor(
      binOk,
      { ZESTROKU_MISSION_RUN_RCH: "1" },
      join(SCRATCH, "mixed-target-zero-run"),
      realExec,
      mixedTargets,
    ));
    expect(byId(cases, "safe-control.snapshot-before-state-audited").outcome).toBe("PASS");
    expect(byId(cases, "safe-control.body-smuggling-refused-closed-schema").outcome).toBe("PASS");
  }, LOCAL_TIMEOUT);

  test("daemon-unavailable local denial remains UNKNOWN", async () => {
    const unavailable: ExecFn = async (argv, options) => {
      if (argv[0] === binOk && argv[1] === "key") {
        return {
          code: 5,
          stdout: "",
          stderr: "zestroku: [DAEMON_UNAVAILABLE] daemon unavailable: Connection refused (os error 61)",
          timedOut: false,
        };
      }
      return realExec(argv, options);
    };
    const cases = await run(ctxFor(binOk, {}, join(SCRATCH, "daemon-unavailable"), unavailable));
    const denial = byId(cases, "safe-control.unconfirmed-denial-zero-post");
    expect(denial.outcome).toBe("UNKNOWN");
    expect(denial.detail).toContain("DAEMON_UNAVAILABLE");
  }, LOCAL_TIMEOUT);

  test("missing binary is UNKNOWN everywhere, never green", async () => {
    const cases = await run(ctxFor("/nonexistent/zestroku", {}, join(SCRATCH, "missing")));
    expect(cases.length).toBeGreaterThan(0);
    for (const c of cases) {
      expect(c.outcome).toBe("UNKNOWN");
    }
  }, LOCAL_TIMEOUT);
});
