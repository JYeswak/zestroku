import assert from "node:assert/strict";
import { createServer, type Server } from "node:http";
import { afterEach, test } from "node:test";
import { evaluateDeviceRecovery, probeDeviceRecovery } from "./recovery-live";

interface DeviceFixture {
  base: URL;
  requests: string[];
}

const servers: Server[] = [];

afterEach(async () => {
  await Promise.all(servers.splice(0).map((server) => new Promise<void>((resolve, reject) => {
    server.close((error) => error ? reject(error) : resolve());
  })));
});

async function serveDevice(infoTier: string, lastSeen: number): Promise<DeviceFixture> {
  const requests: string[] = [];
  const server = createServer((request, response) => {
    requests.push(request.url ?? "");
    response.setHeader("content-type", "application/json");
    if (request.url === "/v1/devices") {
      response.end(JSON.stringify([{
        host: "192.0.2.82",
        name: "Fixture Roku",
        model: "Roku Express",
        model_number: "MODEL-A",
        software_version: "15.2.4",
        access_tier: infoTier,
        last_seen: lastSeen,
      }]));
      return;
    }
    if (request.url === "/v1/devices/192.0.2.82%3A8060/info") {
      response.end(JSON.stringify({
        host: "192.0.2.82",
        model_name: "Roku Express",
        model_number: "MODEL-A",
        software_version: "15.2.4",
        is_tv: false,
        access_tier: infoTier,
        tv_active_channel: null,
      }));
      return;
    }
    response.statusCode = 404;
    response.end(JSON.stringify({ error: "device_not_found" }));
  });
  servers.push(server);
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  return { base: new URL(`http://127.0.0.1:${address.port}`), requests };
}

test("recovery verdict passes when live info and discovery timestamp are current", async () => {
  const fixture = await serveDevice("permissive", 995);
  const observation = await probeDeviceRecovery(fixture.base, "192.0.2.82:8060", 10, 1_000);
  assert.equal(evaluateDeviceRecovery(observation), "PASS");
  assert.deepEqual(fixture.requests, ["/v1/devices", "/v1/devices/192.0.2.82%3A8060/info"]);
});

test("recovery verdict fails stale inventory even when live info succeeds", async () => {
  const fixture = await serveDevice("permissive", 989);
  const observation = await probeDeviceRecovery(fixture.base, "192.0.2.82:8060", 10, 1_000);
  assert.equal(observation.infoOk, true);
  assert.equal(observation.discoveryRecent, false);
  assert.equal(evaluateDeviceRecovery(observation), "FAIL");
  assert.deepEqual(fixture.requests, ["/v1/devices", "/v1/devices/192.0.2.82%3A8060/info"]);
});

test("recovery verdict fails unknown tier despite recent discovery", async () => {
  const fixture = await serveDevice("unknown", 1_000);
  const observation = await probeDeviceRecovery(fixture.base, "192.0.2.82:8060", 10, 1_000);
  assert.equal(observation.discoveryRecent, true);
  assert.equal(evaluateDeviceRecovery(observation), "FAIL");
});
