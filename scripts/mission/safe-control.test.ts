// scripts/mission/safe-control.test.ts
//
// Planted-negative and positive-control tests for safe-control.ts.
//
// Honesty contract (chief M5, bead .14.3 [a]):
// - Fixtures below implement only the OBSERVABLE contract (exit codes,
//   output shapes, real loopback HTTP exchanges). They never stand in for
//   product proof: green-against-fixture shows the checker logic CAN pass,
//   red-against-planted shows it CAN fail. Real product proof comes from
//   dispatcher runs against the real binary and RCH suites.
// - No mocked command echoes as success evidence, no source/file presence
//   checks, no failure-forcing env flag, no regex on product source.
// - Real sockets (loopback only), real subprocesses, real policy strings
//   copied verbatim from product source (mutate.rs, mcp/lib.rs).
// - Never mints approvals, never touches a non-loopback host, never writes
//   outside the test scratch dir (left for the scratch reaper, never rm -rf).

import { describe, test, expect, beforeAll } from "bun:test";
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, chmodSync } from "node:fs";
import { join, resolve } from "node:path";
import { createServer } from "node:http";
import type { Server } from "node:http";
import { run, isCompletedDispatchText, sanitizeDetail } from "./safe-control";
import type { CheckCase, CheckContext } from "../mission-check";

const REPO = resolve(import.meta.dir, "..", "..");
const SCRATCH = join(REPO, "var", "agent-tmp", `safe-control-test.${process.pid}`);
const LOCAL_TIMEOUT = 30_000;
// Full-revision-shaped test input (not a real commit).
const TEST_REV = "f".repeat(40);
const TEST_SHORT = TEST_REV.slice(0, 7);

type ExecResult = { code: number; stdout: string; stderr: string; timedOut: boolean };

function realExec(
  argv: string[],
  options?: { env?: Record<string, string>; timeoutMs?: number },
): Promise<ExecResult> {
  // Exception: this is a kill-switch for real subprocesses, not a test
  // sleep; the timeout never adds latency to a passing run.
  const timeoutMs = options?.timeoutMs ?? LOCAL_TIMEOUT;
  const { promise, resolve } = Promise.withResolvers<ExecResult>();
  const child = spawn(argv[0], argv.slice(1), {
    env: { ...process.env, ...options?.env },
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

function ctxFor(binary: string, env: Record<string, string | undefined>, scratch: string): CheckContext {
  return {
    repo: REPO,
    binary,
    source: TEST_REV,
    scratch,
    env,
    exec: realExec,
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
// exact real stub text from zestroku-mcp with exit 0 and dispatches nothing.
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
    expect(byId(cases, "safe-control.undo-surface").outcome).toBe("FAIL");
    assertNoApprovalOnlyPass(cases);
  }, LOCAL_TIMEOUT);

  test("conforming binary passes local cases, remotes stay UNKNOWN", async () => {
    const cases = await run(ctxFor(binOk, {}, join(SCRATCH, "conforming")));
    expect(byId(cases, "safe-control.source-bound-artifact").outcome).toBe("PASS");
    expect(byId(cases, "safe-control.unconfirmed-denial-zero-post").outcome).toBe("PASS");
    expect(byId(cases, "safe-control.live-approval-boundary").outcome).toBe("PASS");
    for (const id of [
      "safe-control.replay-single-use",
      "safe-control.free-text-denial-no-dispatch",
      "safe-control.snapshot-before-state-audited",
      "safe-control.unknown-principal-closed-body",
      "safe-control.dispatch-failure-recorded-as-failure",
      "safe-control.live-device-read-only",
      "safe-control.undo-surface",
    ]) {
      expect(byId(cases, id).outcome).toBe("UNKNOWN");
    }
    expect(cases.some((c) => c.outcome === "FAIL")).toBe(false);
    assertNoApprovalOnlyPass(cases);
  }, LOCAL_TIMEOUT);

  test("missing binary is UNKNOWN everywhere, never green", async () => {
    const cases = await run(ctxFor("/nonexistent/zestroku", {}, join(SCRATCH, "missing")));
    expect(cases.length).toBeGreaterThan(0);
    for (const c of cases) {
      expect(c.outcome).toBe("UNKNOWN");
    }
  }, LOCAL_TIMEOUT);
});
