# Pinned FastAPI HTTP patch

`fastapi-http` is a local copy of the package from
[`fastapi_rust@cb9d72926434e2ec3e08cced5eae7f7c13168c7d`](https://github.com/Dicklesworthstone/fastapi_rust/tree/cb9d72926434e2ec3e08cced5eae7f7c13168c7d/crates/fastapi-http).
The facade, core, and Asupersync versions stay pinned; only `fastapi-http` is
patched through the workspace `[patch]` table.

## Why this override exists

FastAPI's `drain_connection_tasks` polled active connection handles in a loop
that awaited `asupersync::runtime::yield_now()`. Source inspection of
Asupersync 0.5.0 indicates that, on ZestRoku's single-worker
`RuntimeBuilder::current_thread()` path, the self-waking root may remain runnable
and dispatch tasks without entering the parked worker loop that services reactor
readiness. A loopback partial-body/SIGTERM diagnostic is consistent with this:
after SIGTERM, `cd` was sent successfully, but the admitted HTTP request produced
no response before the 30-second drain timeout. The final post-EOF simulator
snapshot contained the device-info query, with no earlier request-change frame
in the 200 ms polling loop. This source-level explanation remains a hypothesis
until the patched C2/C3 controls and original serve test pass.

The patch replaces the self-waking yield with a 10 ms Asupersync timer between
drain checks, intended to let the current-thread driver park and service socket
readiness without changing the existing 30-second limit or connection lifecycle.

The behavioral proof must remain the real-daemon controls in
`crates/zestroku-cli/tests/serve_drain_controls.rs` plus the original
`crates/zestroku-cli/tests/serve.rs` scenario. The control's diagnostic exit
alone is not a pass: C2/C3 require the response body, successful daemon exit,
closed listener, and stopped admission.

## Provenance and removal

- `Cargo.toml.upstream`: original package manifest from the pinned revision.
- `UPSTREAM_README.md` and `LICENSE`: upstream material retained with the copy.
- `ZESTROKU_PATCH.diff`: the only intentional Rust source delta.
- `Cargo.toml`: standalone manifest; `fastapi-core` remains the same exact Git
  revision and `asupersync` remains registry 0.5.0.

Remove this override when an upstream fastapi revision contains an equivalent
reactor-aware drain wait. Re-run the C2/C3 controls, the original serve test,
workspace Clippy, and the full workspace suite before removing it.
