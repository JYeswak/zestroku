# Mission

zestroku is owner-interop control for the owner's own Roku devices on the owner's own LAN: one Rust binary (`zestroku`) that is both CLI and daemon, built with the frankensonos method.

Constraints (from AGENTS.md): own devices only; mutations (launch, install) need per-operation owner approval; device identifiers, captures and firmware are never committed; no device is ever flashed (firmware lane is download plus static parse); `#![forbid(unsafe_code)]` at every crate root; pure core, I/O behind traits.

Authority: the canonical plan `COMPREHENSIVE_PLAN_FOR_ZESTROKU.md` in the skill-library-growth edge library. The `.beads/` graph is the work queue.
