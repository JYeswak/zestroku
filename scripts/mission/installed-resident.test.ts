import { spawn } from "node:child_process";
import { createServer, type Server } from "node:net";
import { join } from "node:path";
import { test } from "bun:test";
import { run } from "./installed-resident";
import type { CheckCase, CheckContext } from "../mission-check";

interface ExecOptions {
  env?: Record<string, string>;
  timeoutMs?: number;
}

interface ExecResult {
  code: number;
  stdout: string;
  stderr: string;
  timedOut: boolean;
}

function execute(argv: string[], options: ExecOptions = {}): Promise<ExecResult> {
  return new Promise((resolve) => {
    const child = spawn(argv[0], argv.slice(1), {
      env: { ...process.env, ...options.env },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => { stdout += chunk; });
    child.stderr.setEncoding("utf8").on("data", (chunk: string) => { stderr += chunk; });
    // A real child-process deadline is required to bound the external CLI invocation.
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGTERM");
    }, options.timeoutMs ?? 5_000);
    child.once("error", (error) => {
      clearTimeout(timer);
      resolve({ code: 127, stdout, stderr: `${stderr}${error.message}`, timedOut });
    });
    child.once("close", (code) => {
      clearTimeout(timer);
      resolve({ code: code ?? 1, stdout, stderr, timedOut });
    });
  });
}

function caseNamed(cases: CheckCase[], id: string): CheckCase {
  const found = cases.find((entry) => entry.id === id);
  if (!found) throw new Error(`missing checker result ${id}`);
  return found;
}

async function occupiedPort(): Promise<{ server: Server; port: number }> {
  const server = createServer();
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("loopback server did not expose a port");
  return { server, port: address.port };
}

test("installed-resident checker exercises real CLI behavior and a planted busy bind", async () => {
  const binary = process.env.ZESTROKU_MISSION_TEST_BINARY;
  const source = process.env.ZESTROKU_MISSION_TEST_SOURCE;
  const scratch = process.env.ZESTROKU_MISSION_TEST_SCRATCH;
  if (!binary || !source || !scratch) throw new Error("set immutable candidate binary, full source revision, and owned scratch directory");
  const context: CheckContext = {
    repo: process.cwd(),
    binary,
    source,
    scratch,
    env: {},
    exec: execute,
  };
  const cases = await run(context);
  const version = await execute([binary, "--version"]);
  if (version.code !== 0) throw new Error("candidate version command failed");
  if (caseNamed(cases, "cli.source-bound-artifact").outcome !== "PASS") throw new Error("candidate artifact did not identify the supplied source revision");
  if (caseNamed(cases, "store.first-run").outcome !== "PASS") throw new Error("real empty-store CLI path did not succeed");
  if (caseNamed(cases, "resident.bind-conflict").outcome !== "PASS") throw new Error("real occupied-port startup was not rejected");
});

test("a closed configured service endpoint fails both real HTTP checks", async () => {
  const binary = process.env.ZESTROKU_MISSION_TEST_BINARY;
  const source = process.env.ZESTROKU_MISSION_TEST_SOURCE;
  const scratch = process.env.ZESTROKU_MISSION_TEST_SCRATCH;
  if (!binary || !source || !scratch) throw new Error("set immutable candidate binary, full source revision, and owned scratch directory");
  const occupied = await occupiedPort();
  try {
    const context: CheckContext = {
      repo: process.cwd(),
      binary,
      source,
      scratch,
      env: { ZESTROKU_MISSION_SERVICE_URL: `http://127.0.0.1:${occupied.port}` },
      exec: execute,
    };
    const cases = await run(context);
    if (caseNamed(cases, "resident.http-health").outcome !== "FAIL") throw new Error("unreachable configured service did not fail health check");
    if (caseNamed(cases, "resident.http-inventory").outcome !== "FAIL") throw new Error("unreachable configured service did not fail inventory check");
  } finally {
    await new Promise<void>((resolve) => occupied.server.close(() => resolve()));
  }
});

// The real CLI is always called with argv, never through a shell; this path
// points at a fresh store below the runner-owned scratch area.
export const firstRunStorePath = (scratch: string): string => join(scratch, "first-run", "actions.db");
