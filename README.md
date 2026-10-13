# zestroku — owner-interop control for Roku devices on the owner's LAN

A single Cargo workspace, one binary (`zestroku`) that is both CLI and daemon,
mirroring the frankensonos architecture (its muse and method template), applied
to the Roku ECP surface.

## Overview

Owner-interop control for Roku devices on the owner's LAN. Roku exposes an
unauthenticated HTTP surface (ECP on port 8060); app ids are opaque numbers,
so an agent that guesses launches the wrong app on the owner's TV. Every
mutation here is human-led: one approval per operation, audited, with no
standing blanket.

## Build and run on the owner's Mac

Preferred route (no rch worker needed): `gh workflow run darwin.yml --ref main`
builds a native Mach-O arm64 binary on a GitHub macOS runner, prints its
`file`, `shasum -a 256` and `--version` in the run log, uploads it as the
`zestroku-aarch64-apple-darwin` artifact (`gh run download <run-id> -n
zestroku-aarch64-apple-darwin`) and runs the offline tests natively. Clear the
download quarantine with `xattr -d com.apple.quarantine zestroku`. The Contabo
cross-build below is the fallback.

`zestroku` is a Mach-O arm64 binary cross-built on the Contabo workers (plain
`--target` trips rch's os gate; `--config build.target` produces the identical
binary). `ring` compiles C, so the build links through
`bin/zigcc-darwin-adapter.sh` (wired in `.cargo/config.toml`, darwin target
only) onto the fleet's zig shim; a plain `cc` fails with `unrecognized
command-line option '-arch'`. From a clean checkout at commit C:

```
export ZESTROKU_BUILD_SHA=$(git rev-parse HEAD)
export ZESTROKU_BUILD_DIRTY=$([ -n "$(git status --porcelain)" ] && echo true || echo false)
RCH_WORKER=<your lane worker> RCH_REQUIRE_REMOTE=1 rch exec -- \
  cargo build --release -j 2 --locked \
  --config 'build.target="aarch64-apple-darwin"' \
  --config "env.ZESTROKU_BUILD_SHA=\"$ZESTROKU_BUILD_SHA\"" \
  --config "env.ZESTROKU_BUILD_DIRTY=\"$ZESTROKU_BUILD_DIRTY\"" \
  -p zestroku-cli
```

Local Cargo output (`target/`, `target-local2/`, `t-local/`) and native tracker/NTM
runtime caches are ignored, not source or publishable evidence. WAL recovery
journals remain on disk; durable acceptance receipts belong in `docs/evidence/receipts/`.

A cold build takes about 22 minutes; run it unbounded. The result is
`target/aarch64-apple-darwin/release/zestroku`; `file` must report `Mach-O
64-bit executable arm64`. An untargeted artifact is a Linux ELF and is not
valid macOS evidence.
`zestroku mcp` serves stdio tools: `echo`, `list_devices`, `info`,
`active_app`, `apps`, `recent_actions`, and confirmation-gated `key`/`launch`.
Inventory, info, active-app, and apps reads use the daemon API; they never open
direct LAN sockets or fall back to local results.
`zestroku --version` prints
`zestroku <semver> <git-sha7>[-dirty]`; the sha7 equals
`git rev-parse --short C`. A `-dirty` binary is never acceptable evidence.

### Running from agent panes

MCP `list_devices`, `info`, `active_app`, and `apps` use the daemon over
loopback; they fail if it is unavailable rather than falling back to ECP.
Keep the daemon running in a process tree that has LAN permission; for example,
leave this SSH session attached:

```
ssh -o BatchMode=yes localhost \
  'exec ./target/aarch64-apple-darwin/release/zestroku serve --bind 127.0.0.1:8070'
```

For MCP, the default endpoint is `http://127.0.0.1:8070`; `ZESTROKU_DAEMON_URL`
may override it only with `http://<loopback-ip>:<nonzero-port>`. Invalid MCP
endpoints are rejected before startup. `ZESTROKU_DIRECT=1` explicitly selects
direct ECP and requires LAN permission in the CLI's own process tree.

## Architecture (frankensonos §3 pattern)

```
crates/
  zestroku-types   Pure domain vocabulary (Device, App, Key, AccessTier). No I/O.
  zestroku-proto   LANE A: SSDP discovery (roku:ecp ST), ECP client (query/*,
                   keypress/*, launch/*, input/*), XML parsing (pure).
  zestroku-core    LANE B: inventory, access-tier state, reconciliation,
                   durable store. The daemon's brain.
  zestroku-api     LANE C: HTTP control API.
  zestroku-mcp     LANE C: MCP server — agent tools.
  zestroku-cli     LANE C: `zestroku` binary — CLI + `serve` daemon wiring.
```

Design principles (inherited):
- Pure core, I/O at the edges. ECP request/response encoding and XML parsing
  are pure functions, unit-tested against scrubbed fixtures, no network.
- I/O behind traits (`Transport`, `Store`), async runtime wired in one place.
- `#![forbid(unsafe_code)]` at every crate root.
- Access tiers are a domain concept: every command carries the tier it needs
  (open vs permissive); the core refuses out-of-tier commands by construction.

## Current CLI surface

`zestroku devices` discovers devices (SSDP plus optional seeds) and records
them in the store. macOS gates UDP multicast per executable: a freshly built
binary can find zero devices by SSDP while TCP to the ECP port works
(measured 2026-10-11, byte-identical copies behaved differently). Use
`--scan <cidr>` (private ranges, at most a /22, e.g. `--scan 192.168.1.0/24`)
or `--targets <file>` for discovery that needs no multicast grant.
`zestroku info <host>` and `zestroku active-app <host>` use
`ZESTROKU_DAEMON_URL` (default `http://127.0.0.1:8070`) and fail with
`DAEMON_UNAVAILABLE` within 2 s if the daemon is down; they do not fall back to
direct ECP. `info` includes the live tier and channel-surface result (`unknown`
when an optional probe fails). `ZESTROKU_DIRECT=1` explicitly selects direct
ECP; errno 65 is reported as `LAN_BLOCKED_BY_OS_POLICY`.
`zestroku apps <host> [--json]` lists installed apps in name order. It
requires a recorded Permissive tier from `info`, or an explicit
`ZESTROKU_TIER=permissive` override. Limited or unknown tier is refused
locally before `/query/apps` is sent (exit 3). `--json` emits an `apps` array
with `id`, `name`, `type`, `subtype`, and `version`.
App names and versions are not stable across firmware.
`zestroku alias set <alias> <device>`, `zestroku alias rm <alias>`, and
`zestroku alias ls` manage local device labels. Host arguments accept exact
aliases, a unique `tv` or `stick` class target, or a unique device-name prefix.
Ambiguous matches list candidates; mutations never select one implicitly.
`zestroku approve <launch|key> <host> <target>` requires an interactive owner TTY and
a typed action phrase. It prints a random, five-minute, single-use token bound
to the device endpoint and command. Use `zestroku key <host> <KEY> --confirm <token>` or
`zestroku launch <host> <app-id> --confirm <token>` to consume it.
`zestroku approve install <host> sideload [--installer-port <port>]` defaults to port 80.
It binds the token to that exact installer port and `POST /plugin_install`.
`zestroku install <host> <zip> --password-stdin [--installer-port <port>] [--confirm <token>]`
uploads the owner-provided ZIP to the Roku developer installer using Digest auth.
The username defaults to `rokudev`; override it with `--username`. Supply the owner-set
developer password through stdin, not an argument. ZIP contents and credentials are
never stored in the repository or mutation audit record. The response body is checked
for Roku HTML error markers; HTTP 200 alone is not treated as success.
Blank, arbitrary, mismatched, expired, and reused tokens are refused locally
(exit 4) and audited. HTTP and MCP key/launch requests take the same opaque
token string; neither surface can mint approvals.
`zestroku serve [--bind <ip:port>]` exposes `GET /health`,
`GET /v1/devices`, `GET /v1/devices/{host}/info`,
`GET /v1/devices/{host}/active-app`, `GET /v1/devices/{host}/apps`, and the
confirmation-gated `POST /v1/devices/{host}/key` and
`POST /v1/devices/{host}/launch` routes over the shared store on loopback by
default (`127.0.0.1:8070`).
It binds loopback or private-LAN addresses only; a wildcard needs
`--allow-unsafe-bind`, and the Host header must match the bind address. The
info response includes live `access_tier` and `tv_active_channel`; the channel
field is `null` when the optional ECP probe fails.
`zestroku --version` prints `zestroku <semver> <git-sha7>[-dirty]`.

## Ground truth (verified live 2026-10-09)

- ECP `:8060`, no pairing; XML responses.
- Limited mode: `query/device-info`, `query/active-app` 200; `query/apps`,
  `query/icon/*`, `query/media-player` 403; `query/tv-active-channel`,
  `query/rid`, `query/dev-mode` 404 (absent on Express).
- Devices: Express 4K+ (.182), 50RokuTV (.222, bench unit).

## Milestones

M1 discovery ✅ · M2 wire-doc ✅ · M3 tier map ✅ · M4 keypress ✅ (launch
gated per-op) · M5 firmware lane ✅ · M6 cross-device diff ✅ · M7 = this
repo (in progress) · M8 parity features (beaded).

## Limitations

- Recorded 2026-10-10: Zestroku Rust TCP/SSDP requests from an agent pane
  failed to reach LAN devices, while the same binary reached them through
  `ssh -o BatchMode=yes localhost`; `/usr/bin/nc` and `/usr/bin/python3`
  succeeded from the pane. The cause is not established. Run LAN-touching
  commands through the resident daemon or an SSH session.
- Live mutations need an interactive owner terminal approval per operation;
  there is no standing blanket and no approval API for agents to call.
- `zestroku-sim` reproduces the documented wire behavior, not device firmware;
  media-player and icon bytes are placeholders until sampled read-only.
- No live-device behavior runs in CI; live legs are owner-gated opt-in only.
- Doctor checks, policy file, SSE state stream, and web remote are planned,
  not implemented.

## Status

The on-demand `serve` daemon exposes read routes and confirmation-gated
key/launch writes. MCP read tools map to:

| MCP tool | Daemon route |
|---|---|
| `list_devices` | `GET /v1/devices` |
| `info` | `GET /v1/devices/{host}/info` |
| `active_app` | `GET /v1/devices/{host}/active-app` |
| `apps` | `GET /v1/devices/{host}/apps` |

`echo` is a stdio connectivity check; `recent_actions` reads the shared audit
store. MCP `key` and `launch` keep their confirmation and audit behavior, but
this read-tools path does not dispatch mutations. Daemon-backed read failures
return distinct MCP tool errors such as `device_not_found`, `malformed_apps`,
`transport_error`, and `daemon_unavailable`; no direct-ECP or local-result
fallback is used. CLI `info` and `active-app` also use the daemon by default;
direct ECP requires `ZESTROKU_DIRECT=1`. Protocol ground truth lives in
`skill-library-growth/candidates/edge-library/roku/`
(`COMPREHENSIVE_PLAN_FOR_ZESTROKU.md`, `AGENTS.md`, `ECP-WIRE-DOC.md`,
`CROSS_DEVICE_DIFF.md`, and `firmware/FIRMWARE_UPDATE_PATH.md`). Read those
before coding.

## About Contributions

*About Contributions:* Please don't take this the wrong way, but I do not accept outside contributions for any of my projects. I simply don't have the mental bandwidth to review anything, and it's my name on the thing, so I'm responsible for any problems it causes; thus, the risk-reward is highly asymmetric from my perspective. I'd also have to worry about other "stakeholders," which seems unwise for tools I mostly make for myself for free. Feel free to submit issues, and even PRs if you want to illustrate a proposed fix, but know I won't merge them directly. Instead, I'll have Claude or Codex review submissions via `gh` and independently decide whether and how to address them. Bug reports in particular are welcome. Sorry if this offends, but I want to avoid wasted time and hurt feelings. I understand this isn't in sync with the prevailing open-source ethos that seeks community contributions, but it's the only way I can move at this velocity and keep my sanity.
