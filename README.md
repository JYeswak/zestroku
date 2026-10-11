# zestroku — owner-interop control for Roku devices on the owner's LAN

A single Cargo workspace, one binary (`zestroku`) that is both CLI and daemon,
mirroring the frankensonos architecture (its muse and method template), applied
to the Roku ECP surface.

**Status:** CLI live on the owner LAN — `devices`, `apps`, `info`, `active-app`,
`key`, `launch` are implemented; `key` and `launch` have driven a real device
with audit rows (mutations are human-led, confirmed per op). The protocol
ground truth lives in `skill-library-growth/candidates/edge-library/roku/`
(COMPREHENSIVE_PLAN, AGENTS.md lane doctrine, FIRMWARE_UPDATE_PATH.md). Read
those before coding. The `serve` daemon and MCP/API lanes are not started.

## Build and run on the owner's Mac

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

A cold build takes about 22 minutes; run it unbounded. The result is
`target/aarch64-apple-darwin/release/zestroku`; `file` must report `Mach-O
64-bit executable arm64` — treat any untargeted artifact as Linux ELF until
proven otherwise. `zestroku mcp` serves the MCP tools over stdio (`echo`, `list_devices`,
`recent_actions`, and the confirmation-gated `key` and `launch`). It reads the
shared store; it does not open LAN sockets itself.
`zestroku --version` prints
`zestroku <semver> <git-sha7>[-dirty]`; the sha7 equals
`git rev-parse --short C`. A `-dirty` binary is never acceptable evidence.

### Running from agent panes

Until the resident daemon lands, agent panes run the binary over loopback
ssh, never in-pane:

```
ssh -o BatchMode=yes localhost \
  ZESTROKU_TRANSPORT=std ./target/aarch64-apple-darwin/release/zestroku info 192.168.1.182
```

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

`zestroku devices` lists discovered devices from the store.
`zestroku info <host>` fetches device-info and records the device's observed
tier; `zestroku active-app <host>` reads the focused app.
`zestroku apps <host> [--json]` lists installed apps in name order. It
requires a recorded Permissive tier from `info`, or an explicit
`ZESTROKU_TIER=permissive` override. Limited or unknown tier is refused
locally before `/query/apps` is sent (exit 3). `--json` emits an `apps` array
with `id`, `name`, `type`, `subtype`, and `version`.
App names and versions are not stable across firmware.
`zestroku key <host> <KEY> [--confirm <token>]` and
`zestroku launch <host> <app-id> [--confirm <token>]` require per-operation
confirmation and a Permissive tier; without `--confirm` they are refused
locally (exit 4) and logged to the action log as denied.
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
