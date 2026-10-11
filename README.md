# zestroku — owner-interop control for Roku devices on the owner's LAN

A single Cargo workspace, one binary (`zestroku`) that is both CLI and daemon,
mirroring the frankensonos architecture (its muse and method template), applied
to the Roku ECP surface.

**Status:** skeleton. The protocol ground truth lives in
`skill-library-growth/candidates/edge-library/roku/` (COMPREHENSIVE_PLAN,
AGENTS.md lane doctrine, FIRMWARE_UPDATE_PATH.md). Read those before coding.

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

`zestroku info <host>` and `zestroku active-app <host>` use the asupersync ECP
transport; `info` also records the device's observed tier.
`zestroku apps <host> [--json]` lists installed apps in name order. It requires
a recorded Permissive tier from `info`, or an explicit
`ZESTROKU_TIER=permissive` override. Limited or unknown tier is refused locally
before `/query/apps` is sent (exit 3). `--json` emits an `apps` array with `id`,
`name`, `type`, `subtype`, and `version`.
App names and versions are not stable across firmware.
`zestroku key` and `zestroku launch` require per-operation confirmation and a
Permissive tier; otherwise they are refused locally.

## Ground truth (verified live 2026-10-09)

- ECP `:8060`, no pairing; XML responses.
- Limited mode: `query/device-info`, `query/active-app` 200; `query/apps`,
  `query/icon/*`, `query/media-player` 403; `query/tv-active-channel`,
  `query/rid`, `query/dev-mode` 404 (absent on Express).
- Devices: Express 4K+ (.182), 50RokuTV (.222, bench unit).

## Milestones

M1 discovery ✅ · M2 wire-doc (after M3 flip) · M3 flip+map (armed) ·
M5 firmware lane ✅ · M6 cross-device diff (M6-prep landed) · M7 = this repo.
