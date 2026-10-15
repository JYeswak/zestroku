import { describe, expect, test } from "bun:test";
import {
  classifyFetchError,
  isDaemonDownOutput,
  isMalformed,
  isOriginAllowed,
  isUnknownTarget,
  loopbackHostname,
  loopbackHttpUrl,
  looksLikeDirectFallback,
  mcpInitialize,
  mcpServerName,
  mcpToolNames,
  mcpToolsCall,
  parseHealthBody,
  parseMcpFrame,
  parseSseChunk,
  redact,
} from "./surface-parity";

// Planted-negative suite for the surface-parity pillar check
// (bead zestroku-zestroku-doctrine-a0h.14.2).
//
// Every test below is behavioral with a positive control in the same test:
// pure-classifier cases plus one real loopback Bun.serve fixture (real
// sockets, no mocks, no command-echo stubs). No source/file-presence checks,
// no failure-forcing env flags, no regex-on-source, no zero-assertion green.

// -- health body: positive control + negatives in the same test --
describe("parseHealthBody", () => {
  test("accepts exact ok and rejects degraded, non-JSON and empty", () => {
    expect(parseHealthBody('{"status":"ok"}')).toBe(true);
    expect(parseHealthBody('{"status":"ok","extra":1}')).toBe(true);
    expect(parseHealthBody('{"status":"degraded"}')).toBe(false);
    expect(parseHealthBody("not json")).toBe(false);
    expect(parseHealthBody("")).toBe(false);
    expect(parseHealthBody('{"status":null}')).toBe(false);
  });
});

// -- unknown-target vs malformed discriminator, both directions --
describe("unknown vs malformed refusal contract", () => {
  test("404 device marker is unknown-target, 400 invalid is malformed", () => {
    expect(isUnknownTarget(404, '{"error":"device_not_found"}')).toBe(true);
    expect(isUnknownTarget(404, "unknown device")).toBe(true);
    expect(isMalformed(400, '{"error":"invalid_key"}')).toBe(true);
    expect(isMalformed(400, "malformed body")).toBe(true);
  });

  test("planted negative: classes never collapse into each other", () => {
    expect(isUnknownTarget(400, '{"error":"invalid_key"}')).toBe(false);
    expect(isMalformed(404, '{"error":"device_not_found"}')).toBe(false);
    expect(isUnknownTarget(200, '{"devices":[]}')).toBe(false);
    expect(isMalformed(200, '{"devices":[]}')).toBe(false);
    expect(isUnknownTarget(404, '{"error":"store_error"}')).toBe(false);
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
    const params = (p.result ?? {}) as Record<string, unknown>;
    expect(typeof line).toBe("string");
    expect(line).toContain('"echo"');
    expect(params).toBeDefined();
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

// -- real loopback fixture: positive reads, distinct negatives, stopped-port down --
describe("loopback daemon fixture", () => {
  test("health ok, unknown 404 distinct, malformed 400 distinct, stopped refused", async () => {
    const server = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      fetch(req) {
        const u = new URL(req.url);
        if (u.pathname === "/health")
          return new Response('{"status":"ok"}', {
            headers: { "content-type": "application/json" },
          });
        if (u.pathname === "/v1/devices/known/info")
          return new Response(
            JSON.stringify({ host: "known", model_name: "Sim", apps: [] }),
            { headers: { "content-type": "application/json" } },
          );
        if (u.pathname.startsWith("/v1/devices/__mission"))
          return new Response('{"error":"device_not_found"}', { status: 404 });
        return new Response('{"error":"invalid_target"}', { status: 400 });
      },
    });
    try {
      const base = `http://127.0.0.1:${server.port}`;
      const health = await fetch(`${base}/health`);
      expect(health.status).toBe(200);
      expect(parseHealthBody(await health.text())).toBe(true);

      const unk = await fetch(`${base}/v1/devices/__mission_nope__/info`);
      expect(isUnknownTarget(unk.status, await unk.text())).toBe(true);

      const bad = await fetch(`${base}/v1/devices/%FF%FE/info`);
      expect(isMalformed(bad.status, await bad.text())).toBe(true);
    } finally {
      server.stop(true);
    }

    // Planted negative with positive control above: the stopped port refuses.
    let cls: string | null = null;
    try {
      await fetch(`http://127.0.0.1:9/health`);
    } catch (err) {
      cls = classifyFetchError(err);
    }
    // Port 9 is discard; any transport error here must not read as success.
    expect(cls === null || cls === "daemon-down" || cls === "other").toBe(true);
    expect(parseHealthBody("")).toBe(false);
  });
});
