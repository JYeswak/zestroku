# zestroku — AGENTS.md (flywheel doctrine for every agent on this repo)

**Product:** owner-interop control for Roku devices. Rust workspace, one binary
(`zestroku`) that is both CLI and daemon. Muse/method: frankensonos.

**Ground truth:** the canonical plan is
`/Users/josh/Developer/skill-library-growth/candidates/edge-library/roku/COMPREHENSIVE_PLAN_FOR_ZESTROKU.md`
(deep-updated 2026-10-10 to frankensonos parity: crate inventory reconciled to
this workspace, stack decision recorded, §12 user-value expansion, §13 parity
review). Protocol findings live beside it in the same directory
(ECP-WIRE-DOC.md, CROSS_DEVICE_DIFF.md, FIRMWARE_UPDATE_PATH.md). Read before
coding. This repo's `.beads/` graph is the work queue; the plan §10 lists what
every bead must carry.

## Hard rules

1. **ENGLISH ONLY** in every message, report, and commit. Start reports `LANG:EN`.
2. **Own devices, own LAN.** Targets: Express 4K+ (.182), 50" Roku TV (.222, bench).
3. **Both devices are Permissive** (flipped 2026-10-10). Mutations (launch/install)
   still per-op approval from the owner; keypress approved.
4. **Never commit** device identifiers (serial/UDN/MAC/ad-id), captures, firmware.
5. **Never flash a device.** Firmware lane = download + static parse only.
6. `#![forbid(unsafe_code)]` at every crate root. Pure core, I/O behind traits.
7. Shared-tree: `git commit --only` your paths; Agent Mail reservations before
   shared edits; implementer ≠ verifier.
8. Builds go through immutable-source rch with `RCH_WORKER=contabo-1 RCH_REQUIRE_REMOTE=1`;
   use the source-selection recipe under Test discipline. No darwin workers —
   verify with remote check/test, not local artifacts.

## Flywheel coordination contract

| Surface | Role |
|---|---|
| `.beads/` | Work planned/claimed/blocked/closed via `br`. Refresh `br ready` before claiming. |
| Agent Mail | Cross-agent messages + file reservations (project key: this repo's abs path). |
| AGENTS.md | This file — the operating doctrine. |
| Run-and-exit checks | `cargo test` via rch; the tier-gate tests are the regression core. |
| Receipts in git | Mat-test evidence, probe outputs (scrubbed), commit messages as audit trail. |

Worker loop: report DONE/BLOCKED to the coordinator same-turn; never sit idle —
pull the next ready bead; closed glyph = no work.

## Lanes (from the comprehensive plan §9/§10)

- M1 discovery ✅ · M2 wire-doc ✅ · M3 tier map ✅ (Permissive on both
  devices since 2026-10-10) · M4 keypress ✅ live, launch/install + mutation
  gate open (m7e) · M5 firmware lane ✅ (edge library) · M6 diff ✅
  (CROSS_DEVICE_DIFF.md) · **M7 = this repo (in progress)** · M8 = parity
  features (doctor, error codes, read tools, undo, policy, aliases, SSE/web
  remote — beaded).
- Open M7 beads (pull from `br ready`, doctrine epic and M7d store are the
  frontier): m7a transport, m7b discovery, m7c serve (blocked on a+b+d),
  m7d store, m7e mutation gate, m7f sim (the CI-verifiable e2e path — the
  rch workers have no speaker LAN), then the m8 set.

## Stack (DECIDED — Josh, 2026-10-10: the franken stack, mirroring frankensonos FND-DEPS)

asupersync `0.5` (one-version rule: fastmcp pins `=0.5.0`; fastapi and fsqlite
0.4.9 require it — `cargo tree -i asupersync` must show exactly one version;
never path/git-dep asupersync) + fsqlite `=0.4.9` (`async-api`) + fastmcp git
rev `03b5274` (+ `fastmcp-server` beside it) + fastapi git rev `cb9d729`
(default-features off). Edition 2024, toolchain pinned in
`rust-toolchain.toml` (nightly-2026-08-31). The earlier tokio+reqwest wiring
was scaffold drift and is reverted. Hazard receipts (frankensonos §4, inherited
wholesale): fsqlite `!Send` futures → AsyncConnection/32 MiB thread, fastapi
route macros vs `forbid(unsafe_code)`, asupersync h1 HostPolicy allow-list on
every listener. Full recipe: plan §3 in the edge-library canonical copy.

## Test discipline

All compiled checks, tests, Clippy and artifact builds follow the recorded
`.13` policy (comment 297, strengthened 2026-10-16 17:57): pin the saved
source with `--base <full-sha> --clean-overlay --no-overlay`. Replace
`<full-sha>` with the intended commit. For a candidate, replace `--no-overlay`
with one `--overlay-path <path>` per reserved input; include every required
owned change, never the whole dirty checkout.

`RCH_WORKER=contabo-1 RCH_REQUIRE_REMOTE=1 rch exec --base <full-sha> --clean-overlay --no-overlay -- cargo test -j 2 --locked --workspace`
tests all crates. The tier-gate tests (core) and fixture parser tests (proto)
are load-bearing: they encode the Limited/Permissive finding structurally.

Run `cargo fmt --all --check` (non-compiling, allowed locally) before every
commit, and `cargo clippy --workspace --all-targets -- -D warnings` remotely
when a worker has a slot: CI runs fmt, check, clippy, test, one-asupersync,
tokio-ban and identifier-scrub in that order and a failure at one step skips
all the later ones, so a red fmt hides everything behind it (15 consecutive
runs did, 2026-10-10 20:06Z to 22:55Z). A `[test]` tag means a remote
`Remote command finished: exit=0` was observed, not that CI passed. After a
remote failure of the form `cannot find ... in zestroku_core`, `touch` the
core crate's `src/lib.rs` and rerun: rch workers keep a stale fingerprint
when another agent built a different tree on the same worker.
