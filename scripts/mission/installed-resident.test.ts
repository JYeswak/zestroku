import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "bun:test";
import { assessHealth, assessInventory, inspectHealth } from "./installed-resident";

async function withHealthServer<T>(run: (baseUrl: string) => Promise<T>): Promise<T> {
  const server = createServer((request, response) => {
    const path = new URL(request.url ?? "/", "http://127.0.0.1").pathname;
    response.writeHead(200, { "content-type": "application/json" });
    response.end(path === "/bad" ? JSON.stringify({ status: "degraded" }) : JSON.stringify({ status: "ok" }));
  });
  const listening = Promise.withResolvers<void>();
  server.once("error", listening.reject);
  server.listen(0, "127.0.0.1", listening.resolve);
  await listening.promise;
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("loopback HTTP listener did not expose its bound address");
  try {
    return await run(`http://127.0.0.1:${address.port}`);
  } finally {
    const closed = Promise.withResolvers<void>();
    server.close(() => closed.resolve());
    await closed.promise;
  }
}

const expectedDevice = {
  name: "Fixture Roku",
  host: "192.0.2.41",
  model: "Fixture Model",
  model_number: "FIXTURE-1",
  software_version: "1.0",
  access_tier: "permissive",
  last_seen: 1_700_000_000,
};

test("health evaluator accepts a valid response and rejects a reachable unhealthy body", async () => {
  await withHealthServer(async (baseUrl) => {
    const positive = await inspectHealth(`${baseUrl}/health`);
    assert.equal(positive.outcome, "PASS");
    assert.equal(positive.detail, "reachable endpoint returned HTTP 200 with status ok");

    // Same live loopback listener and status; only the health body changes.
    const plantedNegative = await inspectHealth(`${baseUrl}/bad`);
    assert.equal(plantedNegative.outcome, "FAIL");
    assert.equal(plantedNegative.detail, "reachable endpoint violated health contract with HTTP 200");
  });
});

test("inventory evaluator requires the exact expected host and the API array DTO", () => {
  assert.equal(assessInventory(200, [expectedDevice], expectedDevice.host), true);
  assert.equal(assessInventory(200, [{ ...expectedDevice, host: "192.0.2.42" }], expectedDevice.host), false);
  assert.equal(assessInventory(200, { devices: [expectedDevice] }, expectedDevice.host), false);
  assert.equal(assessInventory(200, [], expectedDevice.host), false);
  assert.equal(assessInventory(503, [expectedDevice], expectedDevice.host), false);
  assert.equal(assessInventory(200, [expectedDevice], ""), false);
});

test("health evaluator rejects non-200 and non-ok response bodies", () => {
  assert.equal(assessHealth(200, { status: "ok" }), true);
  assert.equal(assessHealth(200, { status: "degraded" }), false);
  assert.equal(assessHealth(503, { status: "ok" }), false);
  assert.equal(assessHealth(200, null), false);
});
