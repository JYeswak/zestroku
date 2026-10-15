import assert from "node:assert/strict";
import { createServer, type Server } from "node:http";
import { afterEach, test } from "node:test";
import { probeDeviceRecovery } from "./recovery-live";

const servers: Server[] = [];

afterEach(async () => {
  await Promise.all(servers.splice(0).map((server) => new Promise<void>((resolve, reject) => {
    server.close((error) => error ? reject(error) : resolve());
  })));
});

async function serveDevice(info: unknown, lastSeen: number): Promise<URL> {
  const server = createServer((request, response) => {
    response.setHeader("content-type", "application/json");
    if (request.url === "/v1/devices/127.0.0.1%3A8060/info") {
      response.end(JSON.stringify(info));
      return;
    }
    if (request.url === "/v1/devices") {
      response.end(JSON.stringify([{ host: "127.0.0.1", last_seen: lastSeen }]));
      return;
    }
    response.statusCode = 404;
    response.end(JSON.stringify({ error: "not_found" }));
  });
  servers.push(server);
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  return new URL(`http://127.0.0.1:${address.port}`);
}

test("live recovery check accepts current observed info and inventory", async () => {
  const base = await serveDevice({ model_number: "MODEL-A", access_tier: "permissive" }, 995);
  const actual = await probeDeviceRecovery(base, "127.0.0.1:8060", 10, 1_000);
  assert.deepEqual(actual, { infoOk: true, inventoryFresh: true });
});

test("live recovery check rejects stale inventory while info remains reachable", async () => {
  const base = await serveDevice({ model_number: "MODEL-A", access_tier: "permissive" }, 989);
  const actual = await probeDeviceRecovery(base, "127.0.0.1:8060", 10, 1_000);
  assert.deepEqual(actual, { infoOk: true, inventoryFresh: false });
});

test("live recovery check rejects unknown tier despite fresh inventory", async () => {
  const base = await serveDevice({ model_number: "MODEL-A", access_tier: "unknown" }, 1_000);
  const actual = await probeDeviceRecovery(base, "127.0.0.1:8060", 10, 1_000);
  assert.deepEqual(actual, { infoOk: false, inventoryFresh: true });
});
