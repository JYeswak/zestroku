# zestroku mission and done-bar

(Moved verbatim from the retired .flywheel/MISSION.md and .flywheel/GOAL.md, 2026-10-13. Live anchor for directors: mission = the paragraph under "Mission"; done-bar = "Done means" under "Goal".)

## Mission

zestroku is owner-interop control for the owner's own Roku devices on the owner's own LAN: one Rust binary (`zestroku`) that is both CLI and daemon, built with the frankensonos method.

Constraints (from AGENTS.md): own devices only; mutations (launch, install) need per-operation owner approval; device identifiers, captures and firmware are never committed; no device is ever flashed (firmware lane is download plus static parse); `#![forbid(unsafe_code)]` at every crate root; pure core, I/O behind traits.

Authority: the canonical plan `COMPREHENSIVE_PLAN_FOR_ZESTROKU.md` in the skill-library-growth edge library. The `.beads/` graph is the work queue.

## Goal

Working, verified Roku control from one binary: discovery, read verbs, keypress, gated launch/install, a serve daemon (HTTP API + MCP), a loopback sim for CI-verifiable e2e, and the m8 parity set (doctor, error codes, undo, policy, aliases).

Done means: origin/main CI is green on all gates (fmt, check, clippy, test, one-asupersync, tokio-ban, identifier-scrub), each M7 bead's acceptance is re-run by someone other than its implementer, and the live darwin leg shows both owner devices discovered and stored.
