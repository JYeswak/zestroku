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
  originRefusalDistinct,
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
} from "./surface-parity";
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

  test("device/app/state views typed positive and negative", () => {
    expect(
      validDevicesView({ devices: [{ host: "a", last_seen: 7 }] }),
    ).toEqual(["a"]);
    expect(validDevicesView({ devices: [{ host: "a" }] })).toBeNull();
    expect(validDevicesView({ devices: "nope" })).toBeNull();
    expect(
      validAppsView({ apps: [{ id: 1, name: "x", type: "appl" }] }),
    ).not.toBeNull();
    expect(validAppsView({ apps: [{ id: 1 }] })).toBeNull();
    expect(validAppsView({ apps: "nope" })).toBeNull();
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

// -- origin precedence evaluator on exact codes: 403 UNTRUSTED_ORIGIN probe
// vs 403 confirmation_required control; same-code means broken guard --
describe("originRefusalDistinct", () => {
  test("distinct guard refusal passes with live control", () => {
    expect(
      originRefusalDistinct(
        403,
        '{"error":"UNTRUSTED_ORIGIN"}',
        403,
        '{"error":"confirmation_required"}',
      ),
    ).toBe(true);
  });

  test("planted negative: same-code probe means absent guard", () => {
    expect(
      originRefusalDistinct(
        403,
        '{"error":"confirmation_required"}',
        403,
        '{"error":"confirmation_required"}',
      ),
    ).toBe(false);
    expect(originRefusalDistinct(200, '{"ok":true}', 200, '{"ok":true}')).toBe(false);
    expect(originRefusalDistinct(500, "boom", 403, '{"error":"confirmation_required"}')).toBe(false);
    expect(
      originRefusalDistinct(
        403,
        '{"error":"forbidden_origin"}',
        403,
        '{"error":"confirmation_required"}',
      ),
    ).toBe(false);
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

// -- real loopback evaluator fixture (fixture tier): typed reads, distinct
// refusals, origin precedence pair, unknown-body leakage check --
describe("loopback daemon evaluator fixture", () => {
  test("reads agree, refusals distinct, guard decides, unknown leaks nothing", async () => {
    const server = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      fetch(req) {
        const u = new URL(req.url);
        if (u.pathname === "/health")
          return new Response('{"status":"ok"}', {
            headers: { "content-type": "application/json" },
          });
        if (u.pathname === "/v1/devices")
          return new Response(
            JSON.stringify({ devices: [{ host: "known", last_seen: 7 }] }),
            { headers: { "content-type": "application/json" } },
          );
        if (u.pathname === "/v1/devices/known/info")
          return new Response(
            JSON.stringify({
              host: "known",
              model_name: "Sim",
              model_number: "0000",
              software_version: "1.0",
              is_tv: false,
              access_tier: "Permissive",
            }),
            { headers: { "content-type": "application/json" } },
          );
        if (u.pathname === "/v1/devices/known/key" && req.method === "POST")
          return req.headers.get("origin") === "http://127.0.0.1"
            ? new Response('{"error":"confirmation_required"}', { status: 403 })
            : new Response('{"error":"UNTRUSTED_ORIGIN"}', { status: 403 });
        if (u.pathname.startsWith("/v1/devices/__mission"))
          return new Response('{"error":"device_not_found"}', { status: 404 });
        return new Response('{"error":"invalid_device_target"}', { status: 400 });
      },
    });
    try {
      const base = `http://127.0.0.1:${server.port}`;
      const health = await fetch(`${base}/health`);
      expect(health.status).toBe(200);
      expect(parseHealthBody(await health.text())).toBe(true);

      const devs = await (await fetch(`${base}/v1/devices`)).json();
      expect(validDevicesView(devs)).toEqual(["known"]);

      const info = await (await fetch(`${base}/v1/devices/known/info`)).json();
      expect(validInfoView(info)).toBe("Permissive");

      const unk = await fetch(`${base}/v1/devices/__mission_nope__/info`);
      const unkBody = await unk.text();
      expect(isUnknownTarget(unk.status, unkBody)).toBe(true);
      expect(unkBody).not.toContain('"apps"');

      const bad = await fetch(`${base}/v1/devices/%FF%FE/info`);
      expect(isMalformed(bad.status, await bad.text())).toBe(true);

      const control = await fetch(`${base}/v1/devices/known/key`, {
        method: "POST",
        headers: { "content-type": "application/json", origin: "http://127.0.0.1" },
        body: JSON.stringify({ key: "Home", confirm: "" }),
      });
      const probe = await fetch(`${base}/v1/devices/known/key`, {
        method: "POST",
        headers: { "content-type": "application/json", origin: "http://evil.example" },
        body: JSON.stringify({ key: "Home", confirm: "" }),
      });
      expect(
        originRefusalDistinct(
          probe.status,
          await probe.text(),
          control.status,
          await control.text(),
        ),
      ).toBe(true);
    } finally {
      server.stop(true);
    }
  });
});
