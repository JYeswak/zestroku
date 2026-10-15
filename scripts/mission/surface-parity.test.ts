import { describe, expect, test } from "bun:test";
import {
  classifyFetchError,
  isDaemonDownOutput,
  isKnownTier,
  isMalformed,
  isOriginAllowed,
  isOwnedSlot,
  isUnknownTarget,
  loopbackHostname,
  loopbackHttpUrl,
  looksLikeDirectFallback,
  mcpInitialize,
  mcpServerName,
  mcpToolNames,
  mcpToolsCall,
  openapiHasAppsPath,
  isZestrokuVersion,
  parseHealthBody,
  parseMcpFrame,
  parseSseChunk,
  rchSuiteGreen,
  redact,
  errorField,
  mcpToolErrorText,
  validActiveAppView,
  validAppsView,
  validDevicesView,
  validInfoView,
  mcpInventoryHosts,
  run,
} from "./surface-parity";
describe("error-domain distinction", () => {
  test("404 device_not_found is unknown-target, 400 closed-set codes malformed", () => {
    expect(isUnknownTarget(404, '{"error":"device_not_found"}')).toBe(true);
    expect(isMalformed(400, '{"error":"invalid_device_target"}')).toBe(true);
    expect(isMalformed(400, '{"error":"missing_host"}')).toBe(true);
    expect(isMalformed(400, '{"error":"invalid_json"}')).toBe(true);
  });

  test("planted negative: near-miss codes never collapse into each other", () => {
    expect(isUnknownTarget(404, '{"error":"apps_endpoint_not_found"}')).toBe(false);
    expect(isUnknownTarget(404, '{"error":"store_error"}')).toBe(false);
    expect(isUnknownTarget(400, '{"error":"invalid_device_target"}')).toBe(false);
    expect(isMalformed(404, '{"error":"device_not_found"}')).toBe(false);
    expect(isMalformed(400, '{"error":"something_else"}')).toBe(false);
    expect(isMalformed(200, '{"devices":[]}')).toBe(false);
    expect(isUnknownTarget(200, '{"devices":[]}')).toBe(false);
  });
});

// -- binary identity: first word exactly zestroku, no substring match --
describe("isZestrokuVersion", () => {
  test("first-word identity passes, lookalikes fail", () => {
    expect(isZestrokuVersion("zestroku 0.1.0 f333305\n")).toBe(true);
    expect(isZestrokuVersion("Zestroku 0.1.0 abc1234")).toBe(true);
    expect(isZestrokuVersion("not-zestroku 1.0")).toBe(false);
    expect(isZestrokuVersion("")).toBe(false);
    expect(isZestrokuVersion("zestroku-helper 2.0")).toBe(false);
  });
});

describe("errorField", () => {
  test("reads the typed error code, null otherwise", () => {
    expect(errorField('{"error":"tier_refused"}')).toBe("tier_refused");
    expect(errorField('{"error":42}')).toBeNull();
    expect(errorField('{"ok":true}')).toBeNull();
    expect(errorField("not json")).toBeNull();
  });
});
describe("health response semantics", () => {
  test("a reachable but unhealthy daemon never passes liveness", () => {
    expect(parseHealthBody('{"status":"ok"}')).toBe(true);
    expect(parseHealthBody('{"status":"degraded"}')).toBe(false);
    expect(parseHealthBody("not json")).toBe(false);
    expect(parseHealthBody("")).toBe(false);
    expect(parseHealthBody('{"status":null}')).toBe(false);
  });
});

// -- daemon-down classification with positive controls --
describe("classifyFetchError", () => {
  test("refused is daemon-down, abort is timeout, other stays other", () => {
    expect(classifyFetchError(new Error("fetch failed"))).toBe("daemon-down");
    expect(classifyFetchError(new Error("connect ECONNREFUSED 127.0.0.1"))).toBe(
      "daemon-down",
    );
    expect(classifyFetchError(new Error("The operation was aborted"))).toBe(
      "timeout",
    );
    expect(classifyFetchError(new Error("weird teapot 418"))).toBe("other");
  });

  test("planted negative: ECONNREFUSED is never OS-policy fallback", () => {
    expect(looksLikeDirectFallback("connect ECONNREFUSED 127.0.0.1:9")).toBe(false);
    expect(isDaemonDownOutput("connect ECONNREFUSED 127.0.0.1:9")).toBe(false);
    expect(looksLikeDirectFallback("dial tcp: EHOSTUNREACH")).toBe(true);
    expect(isDaemonDownOutput("zestroku: DAEMON_UNAVAILABLE (exit 2s)")).toBe(true);
  });
});

// -- structured view validators: typed positive, mistyped negatives --
describe("structured view validators", () => {
  const info = {
    host: "loop",
    model_name: "Sim",
    model_number: "0000",
    software_version: "1.0",
    is_tv: false,
    access_tier: "Permissive",
    tv_active_channel: null,
  };

  test("isKnownTier admits the four wire tiers only", () => {
    expect(isKnownTier("Open")).toBe(true);
    expect(isKnownTier("permissive")).toBe(true);
    expect(isKnownTier("Limited")).toBe(true);
    expect(isKnownTier("UNKNOWN")).toBe(true);
    expect(isKnownTier("admin")).toBe(false);
    expect(isKnownTier(42)).toBe(false);
    expect(isKnownTier(null)).toBe(false);
  });

  test("validInfoView returns the tier for typed views", () => {
    expect(validInfoView(info)).toBe("Permissive");
    expect(validInfoView({ ...info, access_tier: "Limited" })).toBe("Limited");
  });

  test("planted negative: mistyped info views rejected without regex", () => {
    expect(validInfoView({ ...info, host: 42 })).toBeNull();
    expect(validInfoView({ ...info, is_tv: "yes" })).toBeNull();
    expect(validInfoView({ ...info, access_tier: "admin" })).toBeNull();
    expect(validInfoView({ ...info, model_name: null })).toBeNull();
    expect(validInfoView("garbage")).toBeNull();
    expect(validInfoView(null)).toBeNull();
  });

  const device = {
    name: "Fixture", host: "192.0.2.82", model: "SIM", model_number: "X",
    software_version: "1", access_tier: "Permissive", last_seen: 7,
  };
  test("API inventory is an array of complete device summaries", () => {
    expect(validDevicesView([device])).toEqual(["192.0.2.82"]);
    expect(validDevicesView({ devices: [device] })).toBeNull();
    expect(validDevicesView([{ ...device, last_seen: "7" }])).toBeNull();
    expect(validDevicesView([{ host: device.host, last_seen: 7 }])).toBeNull();
  });
  test("MCP error text cannot masquerade as a device inventory", () => {
    const content = [{ type: "text", text: JSON.stringify([device]) }];
    expect(mcpInventoryHosts({ content })).toEqual(["192.0.2.82"]);
    expect(mcpInventoryHosts({ isError: true, content })).toBeNull();
    expect(mcpInventoryHosts({ content: [{ type: "text", text: '{"error":"192.0.2.82"}' }] })).toBeNull();
  });
  test("installed-app IDs stay strings and preserve leading zeroes", () => {
    const app = { id: "00042", name: "Fixture", type: "appl", subtype: null, version: null };
    expect(validAppsView({ host: "192.0.2.82", apps: [app] })).toEqual([app]);
    expect(validAppsView({ host: "192.0.2.82", apps: [{ ...app, id: 42 }] })).toBeNull();
    expect(validAppsView({ apps: [app] })).toBeNull();
  });
  test("active-app absence is distinct from a mistyped ID", () => {
    expect(validActiveAppView({ host: "a", app_id: null, name: "home" })).toBe(true);
    expect(validActiveAppView({ host: "a", app_id: "1", name: "home" })).toBe(false);
    expect(validActiveAppView({ host: "a" })).toBe(false);
  });

  test("openapi paths-key match is structural, not substring", () => {
    expect(
      openapiHasAppsPath({ paths: { "/v1/devices/{host}/apps": {} } }),
    ).toBe(true);
    expect(openapiHasAppsPath({ paths: {} })).toBe(false);
    expect(openapiHasAppsPath({ paths: "nope" })).toBe(false);
    expect(openapiHasAppsPath(null)).toBe(false);
  });
});


// -- structured MCP tool-error text mirrors the accepted suite accessor --
describe("mcpToolErrorText", () => {
  test("reads isError content text, null otherwise", () => {
    expect(
      mcpToolErrorText({ isError: true, content: [{ text: "device_not_found" }] }),
    ).toBe("device_not_found");
    expect(mcpToolErrorText({ isError: false, content: [{ text: "x" }] })).toBeNull();
    expect(mcpToolErrorText({ content: [{ text: "x" }] })).toBeNull();
    expect(mcpToolErrorText({ isError: true, content: [] })).toBeNull();
    expect(mcpToolErrorText({ isError: true, content: [{ text: 42 }] })).toBeNull();
    expect(mcpToolErrorText(null)).toBeNull();
    expect(mcpToolErrorText("nope")).toBeNull();
  });
});

// -- RCH green evaluator: named line plus nonzero count, else zero-run --
describe("rchSuiteGreen", () => {
  const name = "mcp_stdio_routes_reads_through_daemon_and_preserves_gate";
  const green = [
    `test ${name} ... ok`,
    "test result: ok. 1 passed; 0 failed; 0 ignored",
  ].join("\n");

  test("exact named line plus nonzero count is green", () => {
    expect(rchSuiteGreen(green, name)).toBe(true);
  });
  test("ten successes are not mistaken for a zero-test run", () => {
    expect(rchSuiteGreen(`test ${name} ... ok\ntest result: ok. 10 passed; 0 failed; 0 ignored`, name)).toBe(true);
  });
  test("a similarly named test cannot stand in for the required consumer", () => {
    expect(rchSuiteGreen(`test ${name}_unrelated ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored`, name)).toBe(false);
  });

  test("planted negatives: clean exit without proof is a zero-run", () => {
    expect(rchSuiteGreen("test result: ok. 0 passed; 0 failed", name)).toBe(false);
    expect(rchSuiteGreen("test result: ok. 1 passed; 0 failed", "other_test")).toBe(false);
    expect(rchSuiteGreen("", name)).toBe(false);
    expect(rchSuiteGreen(`test ${name} ... FAILED`, name)).toBe(false);
  });
});

// -- covers-slot validity: owned pillar slots only --
describe("isOwnedSlot", () => {
  test("own pillar slots admitted, foreign and malformed rejected", () => {
    expect(isOwnedSlot("surface.http.positive")).toBe(true);
    expect(isOwnedSlot("cap.errors.negative")).toBe(true);
    expect(isOwnedSlot("surface.cli.positive")).toBe(false);
    expect(isOwnedSlot("cap.key.positive")).toBe(false);
    expect(isOwnedSlot("surface.http")).toBe(false);
    expect(isOwnedSlot("http.positive")).toBe(false);
    expect(isOwnedSlot("")).toBe(false);
  });
});

// -- MCP framing: round-trip positive, truncation negative --
describe("MCP newline-delimited framing", () => {
  test("built initialize parses with id preserved", () => {
    const line = mcpInitialize(1);
    const p = parseMcpFrame(line);
    expect(p.ok).toBe(true);
    expect(p.id).toBe(1);
  });

  test("planted negative: truncated and non-JSON frames are rejected", () => {
    expect(parseMcpFrame('{"jsonrpc":"2.0","id":').ok).toBe(false);
    expect(parseMcpFrame("not json at all").ok).toBe(false);
    expect(parseMcpFrame('{"jsonrpc":"2.0","result":{}}').ok).toBe(false);
    expect(parseMcpFrame('[1,2,3]').ok).toBe(false);
  });

  test("server name extraction honors observed shape only", () => {
    expect(mcpServerName({ serverInfo: { name: "zestroku" } })).toBe("zestroku");
    expect(mcpServerName({})).toBeNull();
    expect(mcpServerName({ serverInfo: { name: 42 } })).toBeNull();
    expect(mcpServerName(null)).toBeNull();
  });

  test("tool call builder keeps name and arguments", () => {
    const line = mcpToolsCall(3, "echo", { text: "mission-parity-probe" });
    const p = parseMcpFrame(line);
    expect(p.ok).toBe(true);
    expect(line).toContain('"echo"');
  });

  test("tool names list positive and missing-tool negative", () => {
    const full = {
      tools: ["echo", "list_devices", "info", "active_app", "apps"].map((name) => ({
        name,
      })),
    };
    const names = mcpToolNames(full);
    expect(names).toContain("echo");
    expect(names).not.toContain("launch");
    expect(mcpToolNames({ tools: "nope" })).toEqual([]);
    expect(mcpToolNames(null)).toEqual([]);
  });
});

// -- SSE: data counts, heartbeats never count --
describe("parseSseChunk", () => {
  test("data lines are events, colon lines are heartbeats", () => {
    const got = parseSseChunk(
      ': keepalive\n\ndata: {"seq":1}\n\n: ping\ndata: {"seq":2}\n\n',
    );
    expect(got.events).toEqual(['{"seq":1}', '{"seq":2}']);
    expect(got.heartbeats).toBe(2);
  });

  test("planted negative: heartbeat-only stream yields zero events", () => {
    const got = parseSseChunk(": keepalive\n\n: ping\n\n");
    expect(got.events).toEqual([]);
    expect(got.heartbeats).toBe(2);
  });
});

// -- Origin rule: exact, foreign, missing --
describe("isOriginAllowed", () => {
  test("exact match passes, foreign fails, missing allowed", () => {
    expect(isOriginAllowed("http://127.0.0.1", "127.0.0.1")).toBe(true);
    expect(isOriginAllowed("http://evil.example", "127.0.0.1")).toBe(false);
    expect(isOriginAllowed(null, "127.0.0.1")).toBe(true);
    expect(isOriginAllowed("", "127.0.0.1")).toBe(true);
    expect(isOriginAllowed("https://127.0.0.1", "127.0.0.1")).toBe(false);
  });
});

// -- loopback guards with boundary cases --
describe("loopback guards", () => {
  test("loopback passes, LAN/wildcard/dns/numeric-overflow refused", () => {
    expect(loopbackHostname("127.0.0.1")).toBe(true);
    expect(loopbackHostname("localhost")).toBe(true);
    expect(loopbackHostname("127.0.0.2")).toBe(true);
    expect(loopbackHostname("192.168.1.5")).toBe(false);
    expect(loopbackHostname("0.0.0.0")).toBe(false);
    expect(loopbackHostname("example.com")).toBe(false);
    expect(loopbackHostname("999.1.1.1")).toBe(false);
    expect(loopbackHostname("::1")).toBe(true);
  });

  test("URL gate keeps loopback http, refuses the rest", () => {
    expect(loopbackHttpUrl("http://127.0.0.1:8070")).toBe("http://127.0.0.1:8070");
    expect(loopbackHttpUrl("http://192.168.1.5:8060")).toBeNull();
    expect(loopbackHttpUrl("https://127.0.0.1:8070")).toBeNull();
    expect(loopbackHttpUrl("not a url")).toBeNull();
  });
});

// -- redact: secrets and LAN IPs scrubbed, loopback kept, long input bounded --
describe("redact", () => {
  test("tokens scrubbed, loopback preserved, LAN masked", () => {
    expect(redact('{"confirm":"abc123"}')).toContain("[redacted]");
    expect(redact('{"confirm":"abc123"}')).not.toContain("abc123");
    expect(redact("using http://127.0.0.1:8070/health")).toContain("127.0.0.1");
    expect(redact("dial 192.168.1.182:8060 refused")).toContain("[host]");
    expect(redact("dial 192.168.1.182:8060 refused")).not.toContain("192.168.1.182");
  });

  test("planted near-miss: long non-matching input stays bounded", () => {
    const nearMiss = `${"a1.".repeat(1500)}!`;
    const out = redact(nearMiss);
    expect(out.length).toBeLessThanOrEqual(4000);
    expect(redact(`confirm=${"x".repeat(5000)}`)).toContain("[redacted]");
  });
});


function httpFixtureContext(url: string) {
  const scratch = process.env.TMPDIR;
  if (!scratch || !scratch.includes("/var/agent-tmp/")) throw new Error("Owned TMPDIR required");
  return {
    repo: process.cwd(), binary: "/nonexistent/zestroku", source: "0".repeat(40), scratch,
    env: { ZESTROKU_DAEMON_URL: url, ZESTROKU_MISSION_RUN_RCH: "0" },
    exec: async () => ({ code: 127, stdout: "", stderr: "CLI intentionally absent in HTTP checker fixture", timedOut: false }),
  };
}

test("never probes a configured device mutation by assuming its guard will refuse", async () => {
  let mutationPosts = 0;
  let selectedHostRead = false;
  const host = "192.0.2.82";
  const server = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    fetch(request) {
      const path = new URL(request.url).pathname;
      if (request.method === "POST" && path.startsWith("/v1/devices/")) {
        mutationPosts++;
        return Response.json({ outcome: "completed" });
      }
      if (path === "/health") return Response.json({ status: "ok" });
      if (path === "/v1/devices") return Response.json([{
        host, name: "Fixture", model: "SIM", model_number: "X",
        software_version: "1", access_tier: "Permissive", last_seen: 7,
      }]);
      if (path === `/v1/devices/${host}/info`) selectedHostRead = true;
      if (path === "/") return new Response("<html><button>Remote</button></html>", { headers: { "content-type": "text/html" } });
      return Response.json({ error: "not_found" }, { status: 404 });
    },
  });
  const base = `http://127.0.0.1:${server.port}`;
  try {
    // Positive control: the isolated counter really observes mutation requests.
    await fetch(`${base}/v1/devices/${host}/key`, { method: "POST" });
    expect(mutationPosts).toBe(1);
    const cases = await run(httpFixtureContext(base));
    expect(selectedHostRead).toBe(true);
    expect(mutationPosts).toBe(1);
    expect(cases.find(item => item.id === "browser-product-surface")?.outcome).toBe("UNKNOWN");
  } finally { server.stop(true); }
});

test("bounds a never-ending SSE body after headers", async () => {
  let forcedClose = false;
  const server = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    fetch(request) {
      const path = new URL(request.url).pathname;
      if (path === "/health") return Response.json({ status: "ok" });
      if (path === "/v1/devices") return Response.json([]);
      if (path === "/v1/events") return new Response(new ReadableStream({
        start(controller) { controller.enqueue(new TextEncoder().encode(": heartbeat\n\n")); },
      }), { headers: { "content-type": "text/event-stream" } });
      return Response.json({ error: "not_found" }, { status: 404 });
    },
  });
  // Real fetch-engine deadline integration: fake JS timers cannot advance its I/O.
  // This safety stop bounds the deliberately broken, headers-only deadline variant.
  const safety = setTimeout(() => { forcedClose = true; server.stop(true); }, 9_000);
  try {
    const cases = await run(httpFixtureContext(`http://127.0.0.1:${server.port}`));
    expect(forcedClose).toBe(false);
    expect(cases.find(item => item.id === "sse-deltas")?.outcome).toBe("UNKNOWN");
  } finally { clearTimeout(safety); server.stop(true); }
}, 12_000);
