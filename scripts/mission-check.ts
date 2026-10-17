import { createHash, randomUUID } from "node:crypto";
import { spawn } from "node:child_process";
import { constants, createReadStream } from "node:fs";
import { lstat, mkdir, open, realpath, writeFile } from "node:fs/promises";
import { basename, dirname, isAbsolute, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

export type PillarId = "installed-resident" | "surface-parity" | "safe-control" | "recovery-live";
export interface CheckCase {
  id: string;
  outcome: "PASS" | "FAIL" | "UNKNOWN";
  detail: string;
  claim: "invariant" | "benchmark" | "slo" | "live-witness";
  tier: "simulated" | "fixture" | "live";
  covers?: readonly string[];
}
export interface ExecResult { code: number; stdout: string; stderr: string; timedOut: boolean }
export interface CheckContext {
  repo: string;
  binary: string;
  source: string;
  scratch: string;
  env: Record<string, string | undefined>;
  exec(argv: string[], options?: { env?: Record<string, string | undefined>; timeoutMs?: number }): Promise<ExecResult>;
  runCargoTest(argv: string[], options?: { env?: Record<string, string | undefined>; timeoutMs?: number }): Promise<ExecResult>;
}

const PILLARS: readonly PillarId[] = ["installed-resident", "surface-parity", "safe-control", "recovery-live"];
const IMPLEMENTATIONS: Record<PillarId, { path: string; sha256: string }> = {
  "installed-resident": { path: "scripts/mission/installed-resident.ts", sha256: "4d785cc0036c52259c1437888c58c8a18ca3be56af5c8d090dcad7b78b0ebf49" },
  "surface-parity": { path: "scripts/mission/surface-parity.ts", sha256: "214d59ed1148ccda064edd64b1a61c964bc408cf47d7284acdf5476f1fc9d59e" },
  "safe-control": { path: "scripts/mission/safe-control.ts", sha256: "05d826b4f5ee1edb4a22c95313a5f7b56bf9f35d33e30d638f8b9147cf880e53" },
  "recovery-live": { path: "scripts/mission/recovery-live.ts", sha256: "f7a470179ddcb4a87560877f62a8bf84442babbb77a12e4adb8548f506f42340" },
};

const OBLIGATIONS: Record<PillarId, readonly string[]> = {
  "installed-resident": ["surface.cli", "surface.setup", "surface.resident", "surface.store", "surface.doctor"],
  "surface-parity": ["surface.http", "surface.mcp-stdio", "surface.mcp-http", "surface.tailnet", "surface.sse", "surface.web", "cap.discovery", "cap.info", "cap.apps", "cap.state", "cap.errors"],
  "safe-control": ["cap.key", "cap.launch", "cap.install", "cap.history", "cap.undo", "cap.policy"],
  "recovery-live": ["surface.simulator", "cap.identity", "cap.recovery", "live.device-witness", "live.tailnet-witness", "live.approved-mutation-witness"],
};
const LIVE_POSITIVE: Readonly<Record<string, true>> = {
  "surface.cli": true, "surface.setup": true, "surface.resident": true,
  "surface.store": true, "surface.doctor": true, "surface.http": true,
  "surface.mcp-stdio": true, "surface.mcp-http": true, "surface.tailnet": true,
  "surface.sse": true, "surface.web": true, "live.device-witness": true,
  "live.tailnet-witness": true, "live.approved-mutation-witness": true,
  "cap.key": true, "cap.launch": true, "cap.install": true, "cap.undo": true,
};

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function isCase(value: unknown): value is CheckCase {
  return record(value) && typeof value.id === "string" && value.id.length > 0 &&
    typeof value.outcome === "string" && ["PASS", "FAIL", "UNKNOWN"].includes(value.outcome) && typeof value.detail === "string" &&
    typeof value.claim === "string" && ["invariant", "benchmark", "slo", "live-witness"].includes(value.claim) &&
    typeof value.tier === "string" && ["simulated", "fixture", "live"].includes(value.tier) &&
    (value.covers === undefined || (Array.isArray(value.covers) && value.covers.every(item => typeof item === "string")));
}

export function summarize(cases: readonly CheckCase[]): number {
  if (cases.length === 0 || new Set(cases.map(item => item.id)).size !== cases.length) return 1;
  if (cases.some(item => !isCase(item) || item.outcome === "FAIL")) return 1;
  return cases.some(item => item.outcome === "UNKNOWN") ? 2 : 0;
}

export function completeCoverage(pillar: PillarId, cases: readonly CheckCase[]): CheckCase[] {
  const completed = [...cases];
  const allowed = OBLIGATIONS[pillar].flatMap(id => [`${id}.positive`, `${id}.negative`]);
  const invalidCoverage = cases.some(item => (item.covers ?? []).some(id => !allowed.includes(id)));
  if (invalidCoverage) completed.push({ id: "coverage:unowned-obligation", outcome: "FAIL", detail: "Case claims an obligation outside its registered pillar", claim: "invariant", tier: "fixture" });
  if (cases.some(item => OBLIGATIONS[pillar].some(id => item.covers?.includes(`${id}.positive`) && item.covers?.includes(`${id}.negative`)))) {
    completed.push({ id: "coverage:combined-positive-negative", outcome: "FAIL", detail: "A single observation cannot stand in for both a positive and its independent negative", claim: "invariant", tier: "fixture" });
  }
  for (const obligation of OBLIGATIONS[pillar]) {
    for (const polarity of ["positive", "negative"]) {
      const id = `${obligation}.${polarity}`;
      // Physical effects require owner-approved positive witnesses. Dangerous
      // refusal/fault controls stay isolated; an actual off-LAN denial is live.
      const needsLive = (polarity === "positive" && LIVE_POSITIVE[obligation]) || obligation === "live.tailnet-witness";
      const observations = cases.filter(item => item.covers?.includes(id));
      if (observations.length === 0 || !observations.some(item => item.outcome === "PASS" && (!needsLive || (item.tier === "live" && item.claim === "live-witness")))) {
        completed.push({
          id: `coverage:${id}`, outcome: "UNKNOWN",
          detail: needsLive ? "Required live behavioral witness is missing or non-green; fixture/simulator evidence cannot replace it" : "Required independent behavioral assertion is missing or non-green; no scope inferred from four summary results",
          claim: needsLive ? "live-witness" : "invariant", tier: needsLive ? "live" : "simulated",
        });
      }
    }
  }
  return completed;
}

async function sha256File(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const bytes of createReadStream(path)) hash.update(bytes);
  return hash.digest("hex");
}
export async function verifyImplementation(path: string, expected: string): Promise<boolean> {
  try {
    const stat = await lstat(path);
    return stat.isFile() && !stat.isSymbolicLink() && stat.size <= 2 * 1024 * 1024 && await sha256File(path) === expected;
  } catch { return false; }
}

export async function captureVerifiedImplementation(repo: string, relativePath: string, expected: string): Promise<{ url: string; sha256: string }> {
  const root = await realpath(repo);
  const path = resolve(root, relativePath);
  if (isAbsolute(relativePath) || !path.startsWith(`${root}${sep}`)) throw new Error("IMPLEMENTATION_OUTSIDE_REPO");
  if (await realpath(path) !== path) throw new Error("IMPLEMENTATION_SYMLINK_REFUSED");
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  let bytes: Buffer;
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > 2 * 1024 * 1024) throw new Error("IMPLEMENTATION_SIZE_OR_KIND");
    const buffer = Buffer.alloc(stat.size + 1);
    let used = 0;
    while (used < buffer.byteLength) {
      const chunk = await handle.read(buffer, used, buffer.byteLength - used, null);
      if (chunk.bytesRead === 0) break;
      used += chunk.bytesRead;
    }
    if (used > stat.size) throw new Error("IMPLEMENTATION_CHANGED_DURING_CAPTURE");
    bytes = buffer.subarray(0, used);
  } finally { await handle.close(); }
  const digest = createHash("sha256").update(bytes).digest("hex");
  if (digest !== expected) throw new Error("IMPLEMENTATION_DIGEST_MISMATCH");
  const source = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  const transpiler = new Bun.Transpiler({ loader: "ts", target: "bun" });
  // These four modules use built-ins only. A new local/helper/package import
  // needs an explicit registered closure, never ambient mutable resolution.
  if (transpiler.scan(source).imports.some(item => !item.path.startsWith("node:") && item.path !== "bun")) throw new Error("UNREGISTERED_IMPLEMENTATION_IMPORT");
  const javascript = transpiler.transformSync(source);
  // Execute the verified in-memory bytes, not a mutable path or scratch copy.
  return { url: `data:text/javascript;base64,${Buffer.from(javascript).toString("base64")}`, sha256: digest };
}

export async function execCommand(argv: string[], options: { cwd: string; env: Record<string, string | undefined>; timeoutMs: number; allowLocalCargo?: boolean }): Promise<ExecResult> {
  if (argv.length === 0 || !argv[0] || !Number.isSafeInteger(options.timeoutMs) || options.timeoutMs <= 0) return { code: 2, stdout: "", stderr: "INVALID_COMMAND_OR_DEADLINE", timedOut: false };
  const executable = basename(argv[0] ?? "");
  const refused: Readonly<Record<string, true>> = { cargo: true, rustc: true, rustdoc: true, rustup: true, rustfmt: true, env: true, sh: true, bash: true, zsh: true, dash: true, sudo: true, xcrun: true };
  // This is a controlled argv boundary, not an OS sandbox for arbitrary code.
  if (Object.hasOwn(refused, executable) && !(executable === "cargo" && options.allowLocalCargo === true)) return { code: 126, stdout: "", stderr: "LOCAL_RUST_OR_SHELL_WRAPPER_REFUSED_USE_REMOTE_RCH_AND_CTX_ENV", timedOut: false };
  if (process.platform === "win32") return { code: 126, stdout: "", stderr: "OWNED_POSIX_PROCESS_GROUP_REQUIRED", timedOut: false };
  const env: Record<string, string> = {};
  for (const [key, value] of Object.entries(options.env)) if (value !== undefined) env[key] = value;
  if (executable === "rch") env.RCH_REQUIRE_REMOTE = "1";
  const { promise, resolve: resolveResult } = Promise.withResolvers<ExecResult>();
  const child = spawn(argv[0] ?? "", argv.slice(1), { cwd: options.cwd, env, detached: true, stdio: ["ignore", "pipe", "pipe"] });
  // detached:true creates a new group whose PGID is this recorded child PID.
  // Only this owned group is signaled, never a pre-existing daemon or peer.
  const ownedGroup = child.pid;
  let timedOut = false;
  let outputExceeded = false;
  let captureFailed = false;
  let settled = false;
  let stopping = false;
  let escalation: NodeJS.Timeout | undefined;
  const stdout: string[] = [];
  const stderr: string[] = [];
  let stdoutBytes = 0;
  let stderrBytes = 0;
  const signalOwned = (signal: NodeJS.Signals) => {
    if (!ownedGroup) return;
    try { process.kill(-ownedGroup, signal); }
    catch { try { child.kill(signal); } catch { /* already exited */ } }
  };
  const stopOwned = () => {
    if (stopping) return;
    stopping = true;
    signalOwned("SIGTERM");
    escalation = setTimeout(() => signalOwned("SIGKILL"), 100);
  };
  const deadline = setTimeout(() => { timedOut = true; stopOwned(); }, options.timeoutMs);
  const finish = (code: number, failure?: string) => {
    if (settled) return;
    settled = true;
    if (stopping) signalOwned("SIGKILL");
    clearTimeout(deadline);
    clearTimeout(escalation);
    resolveResult({
      code: timedOut ? 124 : outputExceeded || captureFailed ? 125 : code,
      stdout: stdout.join(""),
      stderr: failure ?? (outputExceeded ? "OUTPUT_LIMIT_EXCEEDED_NON_GREEN" : captureFailed ? "COMMAND_CAPTURE_FAILED_NON_GREEN" : stderr.join("")),
      timedOut,
    });
  };
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk: string) => {
    stdoutBytes += Buffer.byteLength(chunk);
    if (stdoutBytes > 8 * 1024 * 1024) { outputExceeded = true; stopOwned(); }
    else stdout.push(chunk);
  });
  child.stderr.on("data", (chunk: string) => {
    stderrBytes += Buffer.byteLength(chunk);
    if (stderrBytes > 8 * 1024 * 1024) { outputExceeded = true; stopOwned(); }
    else stderr.push(chunk);
  });
  child.stdout.on("error", () => { captureFailed = true; stopOwned(); });
  child.stderr.on("error", () => { captureFailed = true; stopOwned(); });
  child.once("error", () => finish(127, "EXECUTABLE_UNAVAILABLE"));
  child.once("close", code => finish(code ?? 1));
  return promise;
}

function isPillar(value: string | undefined): value is PillarId {
  return PILLARS.some(id => id === value);
}
export function selectRustExecutor(
  rchRequest: string | undefined,
  rchAvailable: boolean,
): "rch" | "cargo" {
  return rchRequest === "1" || rchAvailable ? "rch" : "cargo";
}
interface MissionExecution {
  rustExecutor: "rch" | "cargo";
  env: Record<string, string | undefined>;
  exec: CheckContext["exec"];
  registrationCase: CheckCase;
}

async function prepareMissionExecution(repo: string, scratch: string): Promise<MissionExecution> {
  const rustExecutor = selectRustExecutor(process.env.ZESTROKU_MISSION_RUN_RCH, Bun.which("rch") !== null);
  const env = { ...process.env, TMPDIR: scratch, ZESTROKU_MISSION_RUN_RCH: rustExecutor === "rch" ? "1" : "0" };
  const exec: CheckContext["exec"] = (argv, options) => execCommand(argv, {
    cwd: repo,
    env: { ...env, ...options?.env, TMPDIR: scratch },
    timeoutMs: options?.timeoutMs ?? 30_000,
    allowLocalCargo: rustExecutor === "cargo" && argv[0] === "cargo",
  });
  const registration = await exec(["omp-kit", "mission", "validate", "--project", repo, "--json"]);
  const available = registration.code === 0 && !registration.timedOut;
  const unavailable = registration.stderr === "EXECUTABLE_UNAVAILABLE";
  const registrationCase: CheckCase = {
    id: "registration-integrity",
    outcome: available ? "PASS" : unavailable ? "UNKNOWN" : "FAIL",
    detail: available
      ? "Canonical mission registration validated"
      : unavailable ? "omp-kit is unavailable; registration integrity cannot be established" : "Canonical mission registration validation is non-green",
    claim: "invariant",
    tier: "fixture",
  };
  return { rustExecutor, env, exec, registrationCase };
}

function createCargoTestRunner(
  source: string,
  rustExecutor: MissionExecution["rustExecutor"],
  exec: CheckContext["exec"],
): CheckContext["runCargoTest"] {
  return (argv, options) => {
    const remote = rustExecutor === "rch";
    const prefix = remote
      ? ["rch", "exec", "--base", source, "--clean-overlay", "--no-overlay", "--", "cargo"]
      : ["cargo"];
    return exec([...prefix, ...argv], {
      env: { ...options?.env, RCH_REQUIRE_REMOTE: remote ? "1" : undefined },
      timeoutMs: options?.timeoutMs,
    });
  };
}

function registrationReadbackStable(initial: CheckCase, after: ExecResult): boolean {
  if (initial.outcome === "PASS") return after.code === 0 && !after.timedOut;
  if (initial.outcome === "UNKNOWN") return after.stderr === "EXECUTABLE_UNAVAILABLE";
  return true;
}



export async function main(args: readonly string[]): Promise<number> {
  if (args.length !== 2 || args[0] !== "--pillar" || !isPillar(args[1])) {
    console.error(JSON.stringify({ outcome: "FAIL", case: "usage", detail: "Use --pillar installed-resident|surface-parity|safe-control|recovery-live" }));
    return 2;
  }
  const pillar = args[1];
  const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const scratchRoot = join(repo, "var", "agent-tmp");
  await mkdir(scratchRoot, { recursive: true, mode: 0o700 });
  const scratch = join(scratchRoot, `mission-check-${randomUUID()}.${process.pid}`);
  await mkdir(scratch, { mode: 0o700 });
  await writeFile(join(scratch, ".owner"), `pid=${process.pid}\nlabel=mission-check\nrepo=${repo}\ncreated=${new Date().toISOString()}\n`, { mode: 0o600, flag: "wx" });
  const { rustExecutor, env, exec, registrationCase } = await prepareMissionExecution(repo, scratch);
  const checkerSha256 = await sha256File(fileURLToPath(import.meta.url));
  const head = await exec(["git", "rev-parse", "HEAD"]);
  const source = env.ZESTROKU_MISSION_SOURCE ?? head.stdout.trim();
  const sourceIsFull = (source.length === 40 || source.length === 64) && [...source].every(character => "0123456789abcdef".includes(character));
  if (head.code !== 0 || !sourceIsFull) {
    console.error(JSON.stringify({ pillar, outcome: "UNKNOWN", case: "source-identity", detail: "A full immutable source revision is required" }));
    return 2;
  }
  const pinnedCommit = await exec(["git", "rev-parse", "--verify", `${source}^{commit}`]);
  if (pinnedCommit.code !== 0 || pinnedCommit.stdout.trim() !== source) {
    console.error(JSON.stringify({ pillar, outcome: "UNKNOWN", case: "source-identity", detail: "Declared source is not a resolvable immutable commit" }));
    return 2;
  }
  const implementation = IMPLEMENTATIONS[pillar];
  const snapshots: Partial<Record<PillarId, { url: string; sha256: string }>> = {};
  try {
    for (const id of PILLARS) {
      const registered = IMPLEMENTATIONS[id];
      if (!registered) throw new Error("IMPLEMENTATION_UNREGISTERED");
      snapshots[id] = await captureVerifiedImplementation(repo, registered.path, registered.sha256);
    }
  } catch {
    console.error(JSON.stringify({ pillar, outcome: "FAIL", case: "implementation-integrity", detail: "Registered source closure is absent, changed, outside the repo or imports unregistered code" }));
    return 1;
  }
  const snapshot = snapshots[pillar];
  if (!implementation || !snapshot) return 1;
  const binary = env.ZESTROKU_MISSION_BINARY ?? "zestroku";
  const runCargoTest = createCargoTestRunner(source, rustExecutor, exec);
  const context: CheckContext = { repo, source, binary, scratch, env, exec, runCargoTest };
  let artifactSha256: string | null = null;
  const artifactIdentity: CheckCase = {
    id: "artifact-input-identity", outcome: "UNKNOWN",
    detail: "Installed artifact digest/source inputs are missing; checker integrity is not product build provenance",
    claim: "invariant", tier: "fixture",
  };
  const binaryPath = binary.includes("/") ? resolve(repo, binary) : Bun.which(binary);
  if (binaryPath) {
    try {
      artifactSha256 = await sha256File(binaryPath);
      const version = await exec([binaryPath, "--version"]);
      const words = version.stdout.trim().split(" ").filter(word => word.length > 0);
      const expectedDigest = env.ZESTROKU_MISSION_ARTIFACT_SHA256?.trim().toLowerCase();
      const expectedSource = env.ZESTROKU_MISSION_ARTIFACT_SOURCE?.trim();
      if (expectedDigest && expectedSource) {
        const matches = expectedDigest === artifactSha256 && expectedSource === source &&
          version.code === 0 && !version.timedOut && words[0] === "zestroku" && words.at(-1) === source.slice(0, 7);
        artifactIdentity.outcome = matches ? "PASS" : "FAIL";
        artifactIdentity.detail = matches
          ? "Observed artifact digest and clean version agree with declared immutable inputs; independent build provenance is still required"
          : "Actual artifact digest/version and declared source inputs disagree";
      }
    } catch {
      artifactIdentity.detail = "Installed artifact bytes or version could not be observed; no source provenance inferred";
    }
  }
  let cases: CheckCase[];
  try {
    // Runtime selection executes only the immutable verified in-memory module.
    const loaded: unknown = await import(snapshot.url);
    if (!record(loaded) || typeof loaded.run !== "function") throw new Error("invalid checker export");
    const result: unknown = await loaded.run(context);
    if (!Array.isArray(result) || !result.every(isCase)) throw new Error("invalid checker cases");
    cases = [registrationCase, artifactIdentity, ...completeCoverage(pillar, result)];
  } catch {
    cases = [registrationCase, { id: "checker-execution", outcome: "FAIL", detail: "Behavioral checker could not complete; no successful result inferred", claim: "invariant", tier: "fixture" }];
  }
  const closureAfter = await Promise.all(PILLARS.map(async id => {
    const registered = IMPLEMENTATIONS[id];
    return registered !== undefined && await verifyImplementation(join(repo, registered.path), registered.sha256);
  }));
  if (closureAfter.some(matches => !matches)) {
    cases.push({ id: "implementation-changed-during-run", outcome: "FAIL", detail: "Source closure changed while executing; discard this run and re-register", claim: "invariant", tier: "fixture" });
  }
  const registrationAfter = await exec(["omp-kit", "mission", "validate", "--project", repo, "--json"]);
  const checkerSha256After = await sha256File(fileURLToPath(import.meta.url));
  if (!registrationReadbackStable(registrationCase, registrationAfter) || checkerSha256After !== checkerSha256) {
    cases.push({ id: "checker-changed-during-run", outcome: "FAIL", detail: "Canonical check registration or dispatcher bytes changed during execution", claim: "invariant", tier: "fixture" });
  }
  const code = summarize(cases);
  console.log(JSON.stringify({ schema_version: 1, pillar, source, repository_head: head.stdout.trim(), artifact_sha256: artifactSha256, bun_version: Bun.version, check_sha256: checkerSha256, check_sha256_after: checkerSha256After, implementation_sha256: snapshot.sha256, outcome: code === 0 ? "PASS" : code === 1 ? "FAIL" : "UNKNOWN", cases, no_claim: "Registration and checker tests do not establish product usability; required live witnesses and non-author acceptance remain mandatory" }));
  return code;
}

if (import.meta.main) process.exitCode = await main(Bun.argv.slice(2));
