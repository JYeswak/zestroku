import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { join } from "node:path";
import {
  captureVerifiedImplementation,
  completeCoverage,
  execCommand,
  summarize,
  verifyImplementation,
} from "./mission-check";
import type { CheckCase } from "./mission-check";

const scratchRoot = process.env.TMPDIR;
if (!scratchRoot || !scratchRoot.includes("/var/agent-tmp/")) {
  throw new Error("Tests require owned repo var/agent-tmp TMPDIR");
}
const scratch = await mkdtemp(join(scratchRoot, "dispatcher-contract-"));
await writeFile(join(scratch, ".owner"), `pid=${process.pid}\nlabel=dispatcher-contract\nrepo=${process.cwd()}\ncreated=${new Date().toISOString()}\n`);
// Owned generated specimens remain for the scratch reaper; never recursively reap scratch here.

const passed = (id: string, covers: readonly string[] = []): CheckCase => ({
  id, outcome: "PASS", detail: "Fixture evaluator input, not product proof", claim: "invariant", tier: "fixture", covers,
});

describe("non-green outcomes cannot be promoted", () => {
  test("distinct successful observations retain a successful bounded verdict", () => {
    expect(summarize([passed("read"), passed("state")])).toBe(0);
  });
  test("an unknown mandatory witness prevents success even beside passing cases", () => {
    expect(summarize([passed("read"), { ...passed("witness"), outcome: "UNKNOWN" }])).toBe(2);
  });
  test("an actual failure outranks unknown prerequisites", () => {
    expect(summarize([{ ...passed("missing"), outcome: "UNKNOWN" }, { ...passed("dispatch"), outcome: "FAIL" }])).toBe(1);
  });
  test("a check that executes no cases cannot be green", () => {
    expect(summarize([])).toBe(1);
  });
  test("duplicate identities cannot conceal a missing independent case", () => {
    expect(summarize([passed("same"), passed("same")])).toBe(1);
  });
  test("fixture evidence cannot satisfy a required live witness", () => {
    const observation = passed("fixture", ["live.device-witness.positive"]);
    const cases = completeCoverage("recovery-live", [observation]);
    expect(cases.some(item => item.id === "coverage:live.device-witness.positive" && item.outcome === "UNKNOWN")).toBe(true);
    // Same observation and obligation, with a correctly typed live witness.
    const liveCases = completeCoverage("recovery-live", [{ ...observation, tier: "live", claim: "live-witness" }]);
    expect(liveCases.some(item => item.id === "coverage:live.device-witness.positive")).toBe(false);
  });
  test("omitting a required negative prevents green despite a real positive record", () => {
    const positive = { ...passed("cli", ["surface.cli.positive"]), tier: "live", claim: "live-witness" } satisfies CheckCase;
    const cases = completeCoverage("installed-resident", [positive]);
    expect(cases.some(item => item.id === "coverage:surface.cli.negative" && item.outcome === "UNKNOWN")).toBe(true);
    const withNegative = completeCoverage("installed-resident", [positive, passed("cli-refusal", ["surface.cli.negative"])]);
    expect(withNegative.some(item => item.id === "coverage:surface.cli.negative")).toBe(false);
  });
  test("one observation cannot claim both sides of a causal obligation", () => {
    const cases = completeCoverage("safe-control", [passed("combined", ["cap.policy.positive", "cap.policy.negative"])]);
    expect(cases.some(item => item.id === "coverage:combined-positive-negative" && item.outcome === "FAIL")).toBe(true);
  });
  test("simulated dispatch cannot satisfy a physical control positive", () => {
    const observation = { ...passed("dispatch", ["cap.key.positive"]), tier: "simulated" } satisfies CheckCase;
    const cases = completeCoverage("safe-control", [observation]);
    expect(cases.some(item => item.id === "coverage:cap.key.positive" && item.outcome === "UNKNOWN")).toBe(true);
    const live = completeCoverage("safe-control", [{ ...observation, tier: "live", claim: "live-witness" }]);
    expect(live.some(item => item.id === "coverage:cap.key.positive")).toBe(false);
  });
  test("a live label without a live-witness claim cannot promote fixture assertions", () => {
    const mislabeled = { ...passed("mislabeled", ["surface.http.positive"]), tier: "live" } satisfies CheckCase;
    expect(completeCoverage("surface-parity", [mislabeled]).some(item => item.id === "coverage:surface.http.positive")).toBe(true);
  });
});

describe("registered implementation closure", () => {
  test("accepts the pinned bytes and rejects an actual edited implementation", async () => {
    const path = join(scratch, "implementation.ts");
    const original = "export const outcome = 'real assertion';\n";
    const digest = createHash("sha256").update(original).digest("hex");
    await writeFile(path, original);
    expect(await verifyImplementation(path, digest)).toBe(true);
    await writeFile(path, "export const outcome = 'different assertion';\n");
    expect(await verifyImplementation(path, digest)).toBe(false);
  });
  test("missing imported code is rejected rather than treated as an empty check", async () => {
    expect(await verifyImplementation(join(scratch, "absent.ts"), "0".repeat(64))).toBe(false);
  });
  test("a captured verified implementation cannot be swapped before loading", async () => {
    const root = join(scratch, "snapshot-repo");
    await mkdir(root);
    const source = "export const observed: string = 'pinned';\n";
    const path = join(root, "checker.ts");
    await writeFile(path, source);
    const digest = createHash("sha256").update(source).digest("hex");
    const captured = await captureVerifiedImplementation(root, "checker.ts", digest);
    await writeFile(path, "export const observed = 'swapped';\n");
    // Dynamic load intentionally exercises the verified-byte boundary, not a known source import.
    const loaded: unknown = await import(captured.url);
    if (typeof loaded !== "object" || loaded === null || !("observed" in loaded)) throw new Error("fixture export missing");
    expect(loaded.observed).toBe("pinned");
  });
  test("matching code outside the declared repository is refused", async () => {
    const root = join(scratch, "boundary-repo");
    await mkdir(root);
    const source = "export const observed = 'same bytes';\n";
    await writeFile(join(root, "inside.ts"), source);
    await writeFile(join(scratch, "outside.ts"), source);
    const digest = createHash("sha256").update(source).digest("hex");
    await expect(captureVerifiedImplementation(root, "inside.ts", digest)).resolves.toHaveProperty("sha256", digest);
    await expect(captureVerifiedImplementation(root, "../outside.ts", digest)).rejects.toThrow("IMPLEMENTATION_OUTSIDE_REPO");
  });
  test("an unregistered runtime helper is rejected before executing it", async () => {
    const root = join(scratch, "import-repo");
    await mkdir(root);
    await writeFile(join(root, "helper.ts"), "export const value = 7;\n");
    const source = "import {value} from './helper.ts'; export {value};\n";
    await writeFile(join(root, "checker.ts"), source);
    const digest = createHash("sha256").update(source).digest("hex");
    await expect(captureVerifiedImplementation(root, "checker.ts", digest)).rejects.toThrow("UNREGISTERED_IMPLEMENTATION_IMPORT");
  });
});

describe("actual child-process boundary", () => {
  test("preserves distinct stdout, stderr and nonzero status", async () => {
    const result = await execCommand([process.execPath, "-e", "console.log('data'); console.error('diagnostic'); process.exit(7)"], { cwd: process.cwd(), env: process.env, timeoutMs: 5_000 });
    expect(result.code).toBe(7);
    expect(result.stdout.trim()).toBe("data");
    expect(result.stderr.trim()).toBe("diagnostic");
    expect(result.timedOut).toBe(false);
  });
  test("escalates when its owned child ignores graceful termination", async () => {
    // Real platform deadline integration: fake timers cannot advance a separate OS process.
    // The child's independent 3s exit is a leak safety net, not a product SLO.
    const source = "import {writeSync} from 'node:fs'; process.on('SIGTERM',()=>{}); writeSync(1,'ready\\n'); setTimeout(()=>process.exit(0),3000);";
    const started = performance.now();
    const result = await execCommand([process.execPath, "-e", source], { cwd: process.cwd(), env: process.env, timeoutMs: 500 });
    expect(result.stdout).toContain("ready");
    expect(result.timedOut).toBe(true);
    expect(result.code).toBe(124);
    expect(performance.now() - started).toBeLessThan(2500);
  });
  test("deadline cleanup reaches an owned descendant holding the output pipe", async () => {
    // Platform process-group integration; the native child's 3s lifetime bounds a broken supervisor.
    const source = "import {spawn} from 'node:child_process'; import {writeSync} from 'node:fs'; process.on('SIGTERM',()=>{}); spawn('/bin/sleep',['3'],{stdio:['ignore','inherit','inherit']}); writeSync(1,'descendant-ready\\n');";
    const started = performance.now();
    const result = await execCommand([process.execPath, "-e", source], { cwd: process.cwd(), env: process.env, timeoutMs: 500 });
    expect(result.stdout).toContain("descendant-ready");
    expect(result.timedOut).toBe(true);
    expect(result.code).toBe(124);
    expect(performance.now() - started).toBeLessThan(2500);
  });
  test("deadline cleanup reaches a descendant after the parent closes its pipes", async () => {
    // Real POSIX group cleanup, including a child which ignores SIGTERM and owns no output pipe.
    // The independent 3s exit also bounds the deliberately broken supervisor variant.
    const descendant = "process.on('SIGTERM',()=>{}); process.send('ready'); setTimeout(()=>process.exit(0),3000)";
    const parent = `import {spawn} from 'node:child_process'; import {writeSync} from 'node:fs'; const child=spawn(process.execPath,['-e',${JSON.stringify(descendant)}],{stdio:['ignore','ignore','ignore','ipc']}); child.on('message',()=>writeSync(1,String(child.pid))); setInterval(()=>{},1000);`;
    const result = await execCommand([process.execPath, "-e", parent], { cwd: process.cwd(), env: process.env, timeoutMs: 700 });
    const ownedPid = Number(result.stdout);
    expect(result.code).toBe(124);
    expect(Number.isSafeInteger(ownedPid) && ownedPid > 0).toBe(true);
    const observed = await execCommand(["/bin/ps", "-p", String(ownedPid), "-o", "stat="], { cwd: process.cwd(), env: process.env, timeoutMs: 1000 });
    const stopped = observed.code === 1 || observed.stdout.trim().startsWith("Z");
    try { expect(stopped).toBe(true); }
    finally {
      if (!stopped) {
        try { process.kill(ownedPid, "SIGKILL"); } catch { /* recorded owned descendant already exited */ }
      }
    }
  });
  test("rejects direct Rust and shell-wrapper paths with the named boundary", async () => {
    for (const argv of [["cargo", "--version"], ["rustc", "--version"], ["env", "cargo", "--version"]]) {
      const result = await execCommand(argv, { cwd: process.cwd(), env: process.env, timeoutMs: 1000 });
      expect(result.code).toBe(126);
      expect(result.stderr).toBe("LOCAL_RUST_OR_SHELL_WRAPPER_REFUSED_USE_REMOTE_RCH_AND_CTX_ENV");
    }
  });
});
