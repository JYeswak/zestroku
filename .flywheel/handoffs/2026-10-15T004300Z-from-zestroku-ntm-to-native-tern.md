# Zestroku product-usability handoff: NTM to fresh native Tern

LANG:EN

**From:** zestroku NTM director (PeachKnoll, former pane `%7`)
**To:** fresh native Tern zestroku orchestrator
**Real-word prefix:** JUNIPER
**Mission anchor (sender):** SHA-256 `7affabf4552ba5a29138cf33fc8e27f2f38ee28076d4e10defecff816f161339` of `.flywheel/MISSION.md` (whole-file digest, not an implemented mission-lock assertion)
**Companion plan:** `.flywheel/research/align-20261014-plan.md`
**Posture:** STATUS
**Block:** current read-path patch has one failing behavioral regression; shared checkout/history/index must be preserved; physical TV is unavailable.

## 1. Read this first

KNOW: Josh is deliberately killing this NTM session and launching a **fresh native Tern** session. This is a durable ownership handoff, not an instruction to resume an old OMP session, restart old panes, or bootstrap watchers.

KNOW: Number-one priority is **a usable product and CLI across CLI, HTTP, and MCP on the owner's local Roku**, not another governance/reporting system. Use existing omp-kit/Agent Mail/br/bv/RCH surfaces; keep four workers usefully occupied; grade independently; send real substrate defects upstream; use classifier-friendly existing labels and receipts. Never manufacture completion by weakening tests or declaring simulator/old-binary proof to be current live proof.

KNOW: Significant progress shipped: readiness/SIGINT fix `f333305128733eae9cf2a23ce02ceac37c603150`, remotely tested and linted, with a successful exact-SHA hosted CI response observed here, and a verified native Darwin artifact plus actual Darwin readiness/shutdown smoke.

KNOW: The next wiring fix is **saved locally, incomplete, and NOT published to origin/main**: `7c5bd5307d3a8040a84b5875e6d3b9e67aaee893` `[pending]`. Its real daemon/apps positive passes, but its closed-daemon negative fails because a client-side tier lookup happens first. Do not mistake that pending commit for the shipped green source.

KNOW: Use the **existing canonical checkout** `/Users/josh/Developer/zestroku`. A fresh remote clone alone will NOT contain the locally preserved pending commit or the shared peers' uncommitted work. Killing NTM does not remove Git objects or the worktree. Do not delete/reinitialize this checkout.

## 2. Safety and non-negotiable boundaries

KNOW: Read the auto-loaded machine/repo AGENTS rules and `.flywheel/MISSION.md`, `.flywheel/GOAL.md`. The product is owner-only control on the owner's LAN. No firmware flashing. No committed serial/UDN/MAC/ad-id, credentials, raw device captures, or firmware images. Keep `#![forbid(unsafe_code)]`, pure core, I/O behind traits, existing asupersync/fsqlite/fastapi/fastmcp stack; no tokio/reqwest replacement.

KNOW: No real device launch/install was performed by this director. Owner per-operation authorization and the real opaque single-use approval path remain mandatory. A generic request to make the product usable is not permission to mint owner approvals or launch an app. A denial smoke is not a successful mutation.

KNOW: **Watchers stay off.** Do not load `com.omp-kit.fleet-watch`, restore prior sessions with `--resume`, or resurrect the old NTM session automatically. Product daemon operation is separate from fleet watchers, but do not leave test daemons impersonating the current installed product.

KNOW: **No shared-tree merge, reset, rebase, stash, autostash, broad add/clean, receipt deletion, or peer-process kill.** A worker retracted an earlier clean merge-tree claim: the proper `merge-tree --write-tree` returned exit 1 with conflicts in `crates/zestroku-cli/tests/mcp_stdio.rs`, `crates/zestroku-mcp/src/lib.rs`, and `crates/zestroku-sim/src/lib.rs`, all live peer lanes. That report was not rerun here. Preserve those files and the staged index.

KNOW: Temp work uses owned repo `var/agent-tmp/<label>.<pid>/`, `.owner`, and exported TMPDIR. Before heavy work check `uptime`, `sysctl -n hw.ncpu`, `sbh check --need 10G`; use nice 10 and RCH remote-only. No local Rust build. One full suite/build at a time per session. Use the shared RCH scheduler, not an old capacity assumption or speculative worker cleanup.

## 3. Exact preservation and lineage

Snapshot before committing/publishing this handoff:

| Class | Object | Meaning |
|---|---|---|
| KNOW | local `HEAD` `7c5bd5307d3a8040a84b5875e6d3b9e67aaee893` | Pending read-path patch saved by a path-only commit; source has a known failing regression. |
| KNOW | pending tree `31c226a4a14b4d58a9179f88ef1f5023e751bf65` | Full local commit tree, including old local lineage; NOT a claim that the entire tree equals current main. |
| KNOW | last observed `origin/main` `f333305128733eae9cf2a23ce02ceac37c603150` | Published readiness/SIGINT source and two receipts. A docs-only handoff publication may advance origin afterward. |
| KNOW | earlier local save `c47cff1` | Local path-only save for readiness-era README/receipts; not the remote readiness commit identity. |
| KNOW | initial paper plan local save `14fe03c` and published `ef93d8a18b9d1728702776041eea24d76348f21a` | Same plan was saved on diverged local lineage and separately published onto remote lineage. Do not infer whole-tree equivalence. |

KNOW: A narrow blob check returned **the same `serve.rs` blob** `f6c4e3cef5789bc2742707cac6c954d7b0b91a56` at `c47cff1` and `f333305`. This proves equality of that file only. Whole-commit/tree equivalence and safe reconciliation remain unverified. A peer retracted a broader subset/equivalence claim; do not revive it.

KNOW: The pending `7c5bd53` commit saved exactly these nine requested paths, without committing the whole shared index:

- `crates/zestroku-cli/src/apps.rs`
- `crates/zestroku-cli/src/daemon_client.rs`
- `crates/zestroku-cli/src/devices.rs`
- `crates/zestroku-cli/src/transport.rs`
- `crates/zestroku-cli/src/serve.rs`
- `crates/zestroku-cli/src/main.rs`
- `crates/zestroku-cli/tests/serve.rs`
- `crates/zestroku-cli/tests/devices_scan.rs`
- `README.md`

KNOW: Commit output was `9 files changed, 151 insertions(+), 232 deletions(-)`. **That is a diff against the old local parent**, not the intended fresh-main publication diff. Some paths already differed between old local history and origin before this task. Inspect the pending path snapshots against a freshly fetched origin; do NOT blindly cherry-pick the whole pending commit or replace a whole fresh main tree with its old local tree.

KNOW: The current pending code has NOT been pushed because doing so would put a known failing test onto previously green main. Preserve it locally for the fresh orchestrator; no hidden branch/stash/worktree was created. The handoff document itself can be published as a docs-only addition without publishing that source.

## 4. Shipped readiness fix and exact evidence

KNOW: Published commit: `f333305128733eae9cf2a23ce02ceac37c603150`, parent `ef93d8a18b9d1728702776041eea24d76348f21a`. Publication diff was explicitly checked as four paths: `serve.rs`, three README behavior lines, and two receipt JSONs. It binds the listener before printing readiness, handles both SIGINT and SIGTERM, and drains the HTTP server.

KNOW: Remote Linux verification before publication:

- Immutable `ef93d8a` baseline plus **only** `crates/zestroku-cli/src/serve.rs` overlay.
- Overlay SHA-256 `ec07da20fc7e2c3f4f7dbb1ddecd93fc5544658a9c09fb103e008771355f0773`.
- Overlay fingerprint `b2493ed0fe7afd1651aa0bb825ddfdc4ac9eec4917c48c0924f7e01d846b5ee2`.
- `cargo test -j 2 --locked --workspace`: contabo-2, **Remote command finished: exit=0 in 52390ms**.
- `cargo clippy -j 2 --locked --workspace --all-targets -- -D warnings`: contabo-2, **remote exit=0 in 603974ms**.
- Local noncompiling `cargo fmt --all --check`: exit 0.
- Actual test results included readiness 2/2; serve 5/5 including original partial-body SIGTERM scenario; drain controls 4/4; MCP stdio reads 2/2; real installed-app HTTP 1/1; devices_store 6/6. This is not proof of positive MCP mutations.

KNOW: Committed and verified receipts:

- `docs/evidence/receipts/ev_0099bd8ed2d3f5393bf54ba0a963fef33593d03fc2d61a04ece0454ef0be07f8.receipt.json` — current-main baseline + one explicit source overlay full workspace test.
- `docs/evidence/receipts/ev_9c89949974619dee4a19fbc0664f3dc5dac78b41ad4df06c3f56540d6ad51df4.receipt.json` — earlier candidate-only readiness 2/2 proof at `066f3d669695891ed7ef96776aa481e30b72dffa`; its limitations deliberately do not claim the full main suite.

### CI contradiction: preserve both observations

KNOW: This director ran `gh run view 38657418664 --json headSha,status,conclusion,jobs,url` and observed **headSha `f333305128733eae9cf2a23ce02ceac37c603150`, completed, success**, with fmt/check/clippy/test/one-asupersync/tokio-ban/identifier-scrub all success, no required skip. URL: https://github.com/JYeswak/zestroku/actions/runs/38657418664 . Earlier `gh run list` also listed that exact run in progress.

KNOW: A peer repeatedly reported that their listing had no f333305 CI run and therefore red-main remained unresolved. That listing was not reproduced here, and its empty result was not silently treated as an authoritative red verdict. Fresh orchestrator should reconcile by **exact repository, full SHA, and run ID**, not a local HEAD, stale `omp-kit ci status` cache, or empty filtered listing. Preserve the contrary report if a fresh primary API response genuinely disagrees.

KNOW: The preceding real failure was run `38626937585` at `ef93d8a`: false readiness on occupied port and SIGINT not exiting. That old run's later invariant/scrub steps were skipped. Do not call it the current revision's status.

## 5. Real Darwin artifact: available now, baseline only

KNOW: A clean immutable f333305 cross-build completed through RCH on contabo-3. No local build and no Darwin worker were used. RCH build ID `30062307857924171`, remote **exit=0 in 2176568ms**; retrieval **8 files, 20753437 bytes**. It took about 36 minutes cold. A cancellation was previewed only; **no cancellation was issued**.

KNOW:

```text
path: /Users/josh/Developer/zestroku/target/aarch64-apple-darwin/release/zestroku
file: Mach-O 64-bit executable arm64
sha256: ffebbbc8bf7b894e325f37b5a4a34cf50e89fe0956fe1d4b4ca8d47a5b5fe1c2
version: zestroku 0.1.0 f333305
-dirty suffix: absent
```

KNOW: The build used `--base f333305... --clean-overlay --no-overlay`, and RCH forced `RCH_BUILD_SOURCE` to that full commit. The `fe0d5151...` fingerprint is the no-overlay fingerprint observed on the immutable runs; a nonempty hash string does not imply worktree edits were overlaid. **This binary does not include pending 042 changes.**

KNOW: Actual Darwin loopback smoke on that binary:

1. Owned isolated-store daemon PID 26724 bound `127.0.0.1:18072`; `/health` returned 200 and `{"status":"ok"}`.
2. `kill -INT 26724` sent only to the recorded owned process; supervisor observed **exit 0**.
3. Subsequent curl to that port returned exit 7, connection refused: listener released.
4. Separate held-socket bind negative on port 55051 returned **exit 1**, `Address already in use (os error 48)`, with **no listening line**.

KNOW: All director-owned proof daemons were stopped: legacy SSH service `roku-live-proof-62344` on 18071, timeout probe on 18070, and current readiness service `roku-f333-readiness-62344` on 18072. Do not assume a resident production daemon exists on 8070.

## 6. Pending 042 implementation: what changed

KNOW: Active bead `zestroku-apps-discover-selected-path-042`, assignee PeachKnoll. Existing labels added from repo vocabulary: `kind:bug`, `area:serve-daemon`, `lane:m7-serve`, `cli`, `http`, `mcp`, `read`, `source-evidence`.

KNOW: Before-state defects, read in published/worktree source and reported by live workers:

- CLI `apps` directly used `send_selected(AsupersyncTransport, ...)`, unlike daemon-backed info/active-app; daemon URL did not select its path.
- SSDP `discover(&AsupersyncTransport, ...)` bypassed the existing std selector.
- `serve.rs` constructed `Arc::new(AsupersyncTransport)` directly, so daemon reads ignored the already-existing `ZESTROKU_TRANSPORT=std` workaround.

KNOW: Pending patch intentionally reuses existing machinery:

- `daemon_client.rs`: typed installed-app response parsing on existing `device_path`/`daemon_get`; returns existing proto InstalledApp data.
- `apps.rs`: defaults to daemon-backed apps; keeps explicit `ZESTROKU_DIRECT=1` for operator direct diagnosis; preserves JSON/text rendering and maps HTTP 403 to tier exit 3.
- `transport.rs`: zero-sized `SelectedTransport` adapter delegates to existing `send_selected`; no new HTTP stack or fallback. Unknown selector values emit a diagnostic and deterministically choose incumbent asupersync.
- `devices.rs` and `serve.rs`: inject that same adapter for SSDP and daemon ECP calls.
- Removed `selector_follows_env`, an existing tautological default-env unit test, and its orphan production-only `--transport-probe` branch in main. Real std fallback loopback round-trip remains. This was not deletion of a meaningful regression to obtain green.
- Existing real daemon CLI test now covers apps JSON; the unavailable-daemon scenario now covers apps as well as info with a reachable simulator and verifies zero direct fallback requests.
- Existing direct-app simulator test explicitly selects `ZESTROKU_DIRECT=1` to retain its intended direct-mode contract.
- README describes the shared selector, daemon-backed apps, explicit direct mode, and the canonical owner-permitted SSH process tree.

KNOW: `xd://lsp` references for `use_std_fallback` were attempted and **server exited unexpectedly with code 0**. Scoped literal reference search was used only after that failure. No claim of a complete language-server graph was made. Owned Rust files were formatted with rustfmt before this run.

## 7. Last actual test: FAIL, and where to resume

KNOW: Last command was a real immutable-baseline + eight explicitly reserved source/test overlays RCH workspace test, **not a moving entire-worktree sync**:

```sh
RCH_REQUIRE_REMOTE=1 RCH_VISIBILITY=verbose nice -n 10 \
  rch exec --base f333305128733eae9cf2a23ce02ceac37c603150 --clean-overlay \
  --overlay-path crates/zestroku-cli/src/apps.rs \
  --overlay-path crates/zestroku-cli/src/daemon_client.rs \
  --overlay-path crates/zestroku-cli/src/devices.rs \
  --overlay-path crates/zestroku-cli/src/transport.rs \
  --overlay-path crates/zestroku-cli/src/serve.rs \
  --overlay-path crates/zestroku-cli/src/main.rs \
  --overlay-path crates/zestroku-cli/tests/serve.rs \
  --overlay-path crates/zestroku-cli/tests/devices_scan.rs \
  -- cargo test -j 2 --locked --workspace
```

Before running, create fresh owned repo scratch with its `.owner` and export TMPDIR there under the machine rules; do not inherit `/var/folders` or reuse the departed director's PID as your owner.

KNOW: contabo-1; **Remote command finished: exit=101 in 123918ms**, total client wall 141.17 s, completed 2026-10-15T00:40:04Z. Overlay fingerprint **`4358bb541661c0e5ba1a1b09bd90d48478818d69a9031d69be02f69cd4e94e52`**. It stopped in CLI tests/serve.rs:

```text
missing_daemon_fails_fast_without_falling_back_to_the_live_simulator ... FAILED
apps: zestroku: no recorded tier for 127.0.0.1 — run `zestroku info 127.0.0.1` first
(or set ZESTROKU_TIER=permissive explicitly)
```

KNOW: The intended negative expects `DAEMON_UNAVAILABLE` and zero direct simulator requests. The real positive **`info_apps_and_active_app_use_the_running_daemon_for_simulator_reads` passed**. Serve suite was 4 passed / 1 failed; the original SIGTERM drain scenario passed. Installed-app HTTP, MCP read stdio, API mutations, CLI mutations, and earlier CLI suites passed before the failure. The whole workspace is **NOT green** and no Clippy run was performed for this 042 source.

INFER: Likely root of this failure is **ordering/fixture semantics**, not a daemon connection fallback: apps calls `access_tier_for` before daemon reachability; the negative seeds a DeviceRow with permissive text but does not record the real tier observation that `recorded_tier_for` requires. The positive calls info first and therefore obtains the observation. Inspect `apps.rs` and core tier storage before changing anything.

Decision to make on resumption: preserve the desired user contract. Prefer making the daemon-backed default use the authoritative daemon tier/approval policy rather than depending on a stale/missing client-side store, while retaining appropriate explicit-direct tier checks. If local precheck is intentionally required, establish the canonical tier observation in the fixture and document precedence. **Do not simply weaken the negative to accept “no recorded tier”, add a permissive override solely to turn it green, or permit silent direct fallback.** Preserve Limited/unknown refusal semantics at the responsible layer and prove the actual user path.

After the narrow fix: run the targeted real `serve` test first on an immutable baseline/owned overlays, then full workspace + strict clippy + fmt. Publish only exact owned paths onto fresh main after accounting for any intervening peer source changes. Obtain an independent grade; do not self-close.

## 8. Real device evidence and limitations

| Class | Surface | Observation / limit |
|---|---|---|
| KNOW | Owner Express `.182` | Live workers report model 3941X2, firmware 15.2.4, permissive; devices/info/active-app read paths succeeded through SSH-localhost; a direct permitted apps read returned 37 rows. |
| KNOW | Owner TV `.222` | Timed out in this director's seed leg; workers subsequently report L2 host-down/dark. Needs owner power/LAN check. Do not infer a software fix or flash anything. |
| KNOW | Agent-pane direct LAN | Old clean binary 98e50d1 returned errno 65 for both seeds. Same binary through documented SSH-localhost route discovered Express; this discriminates process-tree permission/transport behavior from device outage. |
| KNOW | Legacy daemon HTTP | Old isolated daemon 18071 served Express inventory/info/active-app; old CLI still directly dialed LAN despite daemon URL. Clean-but-old does not equal current revision. |
| KNOW | Apps through current daemon path | Worker reported errno 65 when daemon pinned AsupersyncTransport, motivating pending selector wiring. This error was not rerun merely to confirm it. |
| KNOW | Mutation denial | Worker reports key refusal exit 4, audit 6, zero dispatch. This proves refusal only, not an approved key/launch action. |
| KNOW | MCP echo | A worker retracted an initial no-initialize smoke as invalid; valid initialized echo round-trip stands as **old-binary evidence only**. |

KNOW: `docs/LIVE-VERIFY.md:119-120` still contains a historical recipe saying any nonblank `--confirm` token is acceptable. It is obsolete against current opaque owner-minted, single-use token semantics in README/current core. Do not follow it or automate approval minting. This documentation discrepancy was reported to the mutation/live lanes; it was not silently “fixed” by relaxing the gate.

## 9. MCP mutation dispatch: real missing capability, now tracked

KNOW: `crates/zestroku-mcp/src/lib.rs` contains approval-only `key`/`launch` stubs: `decide` authorizes and returns text saying dispatch happens in serve; MutationTool::call opens audit store and invokes that seam but never performs device I/O. Registration exposes both tool names. **Neither approved text nor existing refusal tests prove MCP mutations usable.**

KNOW: Dedicated leaf now exists:

- `zestroku-zestroku-doctrine-a0h.11.5`
- Title: “MCP key/launch execute approved mutations through resident daemon, never approval-only success”.
- Open, unassigned at final snapshot; parent `.11`, actual blocks dependency `.11.3` HTTP mutation path.
- Labels include `area:mcp-mutation-dispatch`, `kind:feature`, `lane:m7-mcp`, `trust:owner-approval`.
- Do not fold it into `.11.4`, which is deliberately read-only MCP work.

KNOW: Existing MCP async daemon client already has loopback URL validation, bounded response body, 2 s timeout, no redirects/no retries, request-context cancellation, and URI segment encoding. Reuse it for approved daemon POSTs; do not open direct LAN sockets or consume an approval locally and then report success without dispatch.

Required positive/negative contract: actual MCP stdio -> actual daemon -> SimDevice key/launch changes state exactly once; one completed audit row contains true before-state and outcome; missing/wrong/expired/replayed tokens dispatch zero; unavailable/failed daemon returns tool error; failed ECP outcome is recorded as failure. No physical mutation claim without point-of-use owner approval.

## 10. Tracker snapshot and next frontier

KNOW: Fresh native tracker snapshot at shutdown preparation (not an assertion that old assignees are live):

| Bead | Status | Tracker assignee / meaning |
|---|---|---|
| `zestroku-apps-discover-selected-path-042` | in_progress | PeachKnoll; pending 7c5bd53, one failing regression above. Fresh orchestrator should explicitly reclaim/hand off after old session ends. |
| `zestroku-m7c-serve-inq.1.2` | in_review | PeachKnoll; shipped f333305, nonauthor acceptance/close still needed. |
| `zestroku-zestroku-doctrine-a0h.11.1` | in_review | Unassigned; discovery grade remains PARTIAL moving-worktree signal, not an exact-current-source pass. |
| `zestroku-zestroku-doctrine-a0h.11.3` | in_progress | josh; HTTP mutations, original owner/custody not reconciled. Do not close merely because global CI is green. |
| `zestroku-zestroku-doctrine-a0h.11.4` | in_progress | josh; read-only MCP wiring, distinct from mutation dispatch. |
| `zestroku-zestroku-doctrine-a0h.11.5` | open, dependency-blocked | Unassigned; dedicated MCP mutation dispatch. |
| `zestroku-m7c-serve-inq.1.1` | in_progress | OrangeOx; original partial-body drain regression, do not weaken original scenario/controls. |
| `zestroku-m7c-serve-inq.1` | in_progress | Unassigned; parent graceful drain work. |
| `zestroku-zestroku-doctrine-a0h.7` | in_progress | josh; shared checkout/custody reconciliation. |
| `zestroku-zestroku-doctrine-a0h.13` | in_progress | OrangeShore; RCH pooled-target growth/capacity. |
| `zestroku-zestroku-doctrine-a0h.3` | blocked | Unassigned; installer strict-Clippy work. |
| `zestroku-zestroku-doctrine-a0h.12` | blocked | Unassigned; SSDP scope trust-boundary work. |
| `zestroku-zestroku-doctrine-a0h.5` | blocked | Unassigned; native closed-row evidence amendment. |
| `zestroku-self-healing-identity-7ac` | in_review | OrangeShore. |
| `zestroku-zestroku-doctrine-a0h.2` | in_review | PeachStork; gated installer, not automatically approved for live use. |
| `zestroku-lot` | ready/open | CLI --help exits 2, P3; do not take main.rs while the 042 owner is editing it. |
| `zestroku-zestroku-doctrine-a0h.6` | ready/open | External native closed-row amendment release; do not consume all product capacity on an owner/maintainer-dependent item. |

KNOW: Last discovery grade callback explicitly demoted itself: CLI 11/11 and core 5/5 remotely were **PARTIAL worktree-signal because the tree moved**. A `.11.1` comment lists remaining gates. Preserve demotion; no own-bead closure, no receipt promotion.

Recommended order after safety preflight: finish 042 actual default-daemon apps contract; independently close shipped readiness once all original acceptance is observed; get a clean revision-bound discovery grade to unblock `.11.2` daemon startup/empty-store persistence; reconcile the HTTP mutation author handoff and implement `.11.5`; then resident-daemon installation and remaining parity features. Keep independent workers on non-overlapping code/verification lanes.

## 11. Shared index / foreign dirt: preserve verbatim

KNOW: `git status --short` before handoff-doc creation reported **12 staged, 17 unstaged, 26 untracked**. **These are not all this director's work.** The nine owned implementation paths above are saved in 7c5bd53; the remaining paths below have **UNKNOWN/MIXED custody in this handoff**, not permission for Tern to commit, restore, or delete them. Tracker assignees are not file reservations. Re-derive actual holders using authoritative reservations/custody before touching any path.

KNOW: `.beads/issues.jsonl` is mixed: this session changed `.1.2` status/comments and 042 claim/labels/notes, while other workers created/updated rows. SQLite/JSONL are persistent on disk, but a coordinated tracker publication is still outstanding. Preserve DB/WAL/JSONL and never hand-edit rows/SQL to evade native tracker policy.

The staged index belongs to the actors who staged its contents; **do not use `git add -A`, reset, restore, bare commit, stash, or broad receipt re-addition**. In particular, staged receipt deletions and same-path untracked files can coexist: do not “repair” that mechanically.

Complete observed inventory, all UNKNOWN/MIXED custody unless separately verified:

```text
MM .beads/issues.jsonl
 M Cargo.lock
MM crates/zestroku-api/src/lib.rs
MM crates/zestroku-api/src/routes.rs
 M crates/zestroku-api/tests/installed_apps_contract.rs
MM crates/zestroku-cli/src/install.rs
 M crates/zestroku-cli/tests/devices_store.rs
MM crates/zestroku-cli/tests/installed_apps_api.rs
 M crates/zestroku-cli/tests/mcp_stdio.rs
 M crates/zestroku-cli/tests/mutation_cli.rs
 M crates/zestroku-core/Cargo.toml
 M crates/zestroku-core/src/ssdp_io.rs
 M crates/zestroku-core/tests/ssdp_sweep_loopback.rs
 M crates/zestroku-mcp/src/lib.rs
 M crates/zestroku-sim/src/lib.rs
 M docs/LIVE-VERIFY.md
 M docs/SIM-DESIGN.md
D  docs/evidence/receipts/ev_33c7c4a518e2344d066cf1e9000cc7c2eb545657ffaad7a945f0bd06cf22810f.receipt.json
D  docs/evidence/receipts/ev_4ce7eb332e7ccf6dc356ecae24a14d0afe6a69e102d69c451a019f9467a4b2a5.receipt.json
D  docs/evidence/receipts/ev_796b73a673546e214b94077b78faa12674034435ad6e1235aedb713ef65dbc76.receipt.json
D  docs/evidence/receipts/ev_85b1a075b1e2206c3b6c1846a6d4e2a429a345280f9f6b2c15250c1d506c7837.receipt.json
D  docs/evidence/receipts/ev_bec4350fa931565546d1a59a17a3535a7493b19df9e1eb08ee485674b9ab073e.receipt.json
D  docs/evidence/receipts/ev_c58853dd6a6348a7325176ae29cdaaacc3b7760aa3208a756b5aaefd50bc7e60.receipt.json
D  docs/evidence/receipts/ev_f00de83d453d700ff6e40cb8eb6facc33a5dddf13292e4f5cd1b76ea021ae300.receipt.json
?? -?
?? WATCHDOG.yml
?? crates/zestroku-api/tests/common/
?? crates/zestroku-api/tests/mutation_gate_contract.rs
?? crates/zestroku-api/tests/mutation_outcome_contract.rs
?? crates/zestroku-api/tests/mutation_schema_contract.rs
?? crates/zestroku-cli/src/inventory.rs
?? crates/zestroku-cli/tests/apps_cli.rs
?? crates/zestroku-cli/tests/mutation_api.rs
?? crates/zestroku-cli/tests/serve_drain_controls.rs
?? docs/evidence/receipts/ev_00f4f8321aa7767711c483c8f595e4b1c8eda20fbd70c6e2cdbdb8b7f0ec2dc5.receipt.json
?? docs/evidence/receipts/ev_33c7c4a518e2344d066cf1e9000cc7c2eb545657ffaad7a945f0bd06cf22810f.receipt.json
?? docs/evidence/receipts/ev_4ce7eb332e7ccf6dc356ecae24a14d0afe6a69e102d69c451a019f9467a4b2a5.receipt.json
?? docs/evidence/receipts/ev_796b73a673546e214b94077b78faa12674034435ad6e1235aedb713ef65dbc76.receipt.json
?? docs/evidence/receipts/ev_7ba2b45774589d27b682d292ab942651c25f366e54417b24bf9b4c0fb937a3c6.receipt.json
?? docs/evidence/receipts/ev_7ceca8b46bd1581cd452694d29a2d79278fc65f18488f61fa0a09529dd55c1bb.receipt.json
?? docs/evidence/receipts/ev_85b1a075b1e2206c3b6c1846a6d4e2a429a345280f9f6b2c15250c1d506c7837.receipt.json
?? docs/evidence/receipts/ev_9b24c4690b0db798e854d78b74424e329946dd43efde06dac63534690d5ab772.receipt.json
?? docs/evidence/receipts/ev_ab137862c24ed1b8a03336863dbc0fedc304361523f982a6f5b15bddfee124fb.receipt.json
?? docs/evidence/receipts/ev_bdc941215c164480a9654052cdd0e6fe74880dc26e290a159047a5a69a044b42.receipt.json
?? docs/evidence/receipts/ev_bec4350fa931565546d1a59a17a3535a7493b19df9e1eb08ee485674b9ab073e.receipt.json
?? docs/evidence/receipts/ev_c58853dd6a6348a7325176ae29cdaaacc3b7760aa3208a756b5aaefd50bc7e60.receipt.json
?? docs/evidence/receipts/ev_e92de49378cfbc69c6f52bdfc49f796d64990cec80c78dd8339c7fe80b7d4816.receipt.json
?? docs/evidence/receipts/ev_f00de83d453d700ff6e40cb8eb6facc33a5dddf13292e4f5cd1b76ea021ae300.receipt.json
?? docs/evidence/receipts/ev_f9c52bb132a59fca2cf226776e5c264865940e12b45337c55bab8d37f7e8ec59.receipt.json
?? vendor/
```

KNOW: Another worker reported a roughly 952 MB Linux ELF at `target/debug/zestroku` from an RCH artifact event; it was retained pending deletion authorization. Do not execute it on macOS or delete it/the receipt files without the owning safety procedure. Use `file` plus `--version` on the explicit Darwin artifact instead.

## 12. Native surfaces, known friction, and upstream routing

KNOW: First use native Tern's actual topology/identity surfaces; old tmux `%N` IDs are historical only. Prior NTM mapping was director `%7`, CoralDuck `%9`, TurquoiseCondor `%8` (legacy identity previously OrangeOx), PeachStork `%59`, OrangeShore `%6`. Some Agent Mail pane bindings returned `legacy-unverified`; do not treat a reused name as cryptographic proof or as permission to self-grade another session's authored work.

KNOW: Chief durable contact was Agent Mail `project_key=/Users/josh/Developer/omp-kit`, recipient `RedMaple`. Last explicit wake instruction was Herdr `w4:p1`, text then Enter. The old fleet registry simultaneously named `omp-test/%3`; do not blindly reuse either after migration. This process had no `HERDR_ENV=1`, so it did not use Herdr after loading the Herdr environment guard. Derive the current native Tern chief route rather than sending to a guessed pane.

KNOW: Relevant durable messages:

- AM53682: initial zestroku RESEARCH warn; chief acknowledged it. This repo was never reassigned to omp-kit.
- AM53693/53699: paper plan existence/publication.
- AM53727: NTM health/version contradiction sent to existing `ompkit-fp72.43`; kit tracker held by QuietBadger, so evidence was routed to the holder rather than editing around a lease.
- AM53740: dedicated MCP mutation-dispatch leaf contract; now exists as `.11.5`.
- AM53750/53751: readiness publication and native-land failure details.
- AM53767: exact f333305 all-seven-gates CI success response and read-path ownership.
- AM53769: native flow metrics, truthful 0/0 event-rate denominator.
- AM53775: canceled premature grade dispatch because `.11.3` was still in_progress, not review/DONE-claimed. Its delivery before cancellation was UNKNOWN; do not treat any residual draft as closure authorization.
- AM53791: no shared merge; narrow blob check only; contradictory CI listing preserved.
- AM53814: exact Darwin artifact and actual native readiness smoke.

KNOW: `omp-kit land` exists. It initially refused OWNER_UNKNOWN because AGENT_NAME was absent. Source inspection showed it explicitly requires AGENT_NAME and compares it to `am agents resolve-pane`; setting the actual resolver-matched `AGENT_NAME=PeachKnoll` advanced through those checks to **BASELINE_DIVERGED**, because local HEAD was not fetched origin/main. Do not invent an owner name or weaken the guard. The exact existing shared-tree divergence is an upstream reconciliation problem, not a reason to write a project-local lander.

KNOW: Readiness and the earlier paper plan were published using the standing no-shared-merge private-index Git recipe, after checking exact candidate path scope, with ordinary non-force pushes. No merge/rebase/reset or foreign index staging was used. The fresh orchestrator should prefer the current kit mechanism once its actor/baseline requirements are met and coordinate any reconciliation through existing kit owners. Do not assume this old checkout can satisfy native land's HEAD==origin requirement without accounting for all local/peer work.

KNOW: Native `ntm --robot-version` reported `tmux_available=false` while fresh health/tail succeeded. Health labeled an actively working pane error/crash. This contradiction is routed to existing upstream `ompkit-fp72.43`, not “fixed” with a local classifier. `omp-kit send` safely withheld Enter once when a message remained in a busy composer; a drop copy exists, not delivery proof. No broadcast/restart was used to manufacture idle workers.

KNOW: Final RCH queue read at shutdown preparation had `active_builds=[]`, `queued_builds=[]`, four healthy workers, eight available slots (it also reported busy counters despite empty active jobs; do not infer a process to kill). All director-owned builds had completed. Re-read this after restart; old pressure/slot observations expire.

## 13. First actions for the new orchestrator

1. Read this document, mission/goal, current AGENTS, and the canonical edge-library Roku plan. Treat `.flywheel/STATE.md` dated October 11 as historical, not current health. Do not restore an old session.
2. Establish fresh native Tern topology and Agent Mail identity; query existing live reservations/custody. Old leases from this director are being released as part of handoff; acquire exact paths before editing. Do not reuse old pane IDs or assume current owners from generic `josh` assignees.
3. In the canonical checkout, inspect `git rev-parse HEAD origin/main` and **read** status. Preserve pending `7c5bd53`, the shared index, and every foreign/unknown path. Fetching origin is read-safe for worktree contents; merging/resetting is not.
4. `br show zestroku-apps-discover-selected-path-042 --json`; explicitly hand off/reclaim the old session's one implementation claim. Read the actual failed output/case above and fix the tier-before-daemon ordering/fixture contract without weakening the negative.
5. Verify through RCH at an immutable source and explicit owned overlays. First targeted `cargo test -j 2 --locked -p zestroku-cli --test serve -- --nocapture`; then full workspace and strict all-target Clippy; local fmt check. Bind the proof to source hashes/overlay receipt, not a moving worktree. Inspect fresh capacity before worker selection; do not fall back locally.
6. Publish the repaired 042 source only after proof, retaining all unrelated current main paths and peer changes. Build a **new** clean revision-bound Darwin artifact; the existing f333305 artifact cannot prove the new wiring. Confirm Mach-O arm64, full-source provenance, SHA-256, and no dirty suffix.
7. On the powered Express, exercise actual CLI, HTTP, and initialized MCP read calls through the LAN-permitted resident daemon with the selected transport. Prove missing-daemon refusal without direct fallback. Keep live-device and simulator evidence separate. TV success waits for owner power/LAN availability; do not fake “both devices” completion.
8. Reconcile stale HTTP mutation ownership, then assign `.11.5` to an available worker using its original positive and planted-negative contract. Keep `.11.4` read-only scope intact. Re-run every acceptance before nonauthor close; no self-close or metric farming.
9. Keep the four workers on useful independent product code, review, or researched blockers. Root graph previously had zero cycles/isolated/hidden nodes, so do not spend a whole cycle “repairing” an unproven DAG problem. Fix actual no-fix/stale-claim issues with evidence.
10. Report current capability and actual evidence to the chief; every recurring substrate fault goes to existing omp-kit owners/beads. No new local watcher/lander/dispatch framework.

### Useful exact read-only commands

```sh
br ready --json
bv --robot-triage
br show zestroku-apps-discover-selected-path-042 --json
br show zestroku-zestroku-doctrine-a0h.11.5 --json
rch queue --json
rch status --json
gh run view 38657418664 --repo JYeswak/zestroku --json headSha,status,conclusion,jobs,url
file target/aarch64-apple-darwin/release/zestroku
target/aarch64-apple-darwin/release/zestroku --version
```

KNOW: The native 24 h flow snapshot at 00:08Z was: closed_events=0, blocked_events=0, created_events=2, stale_in_progress=6, blocked_no_fix=1, dirt_24h=42, dag_isolated=0, dag_cycles=0, hidden_by_parent=0, wip_multi=1, panes_busy=3, panes_idle=1. The event blocker-to-close rate was **undefined (0/0)**, not zero; blocked stock 3 is pressure, not an event rate. Do not optimize close counts by shrinking product acceptance.

## 14. Honesty / continuation disposition

KNOW: No bead was self-closed. No owner device mutation was performed by this director. No test was weakened to obtain a green result. A tautological test and orphan production test hook were removed in the pending patch, with stronger real default-daemon positive and no-direct-fallback negative retained; that negative currently fails and is disclosed. No snapshot/golden was regenerated. Simulated tests are not live Roku proof. No baseline artifact is represented as containing the pending wiring fix. No whole-tree or whole-commit equivalence is claimed. No shared merge/reset/rebase was performed.

KNOW: One premature `.11.3` review/close dispatch was canceled and corrected in AM53775 because the task was still in_progress. The exact pre-cancel message delivery was unknown, not invented. An earlier inference that a peer's displayed bare cargo command proved local compilation was corrected in AM53735 after the RCH queue showed a corresponding remote job.

KNOW: The owner-requested handoff interrupts product execution. Remaining source work, independent closures, full current live read-surface proof, resident-daemon installation, and the rest of the backlog are **not complete**. The correct next move is the narrow failing 042 contract, not another mission-alignment paper pass.

This handoff's consumer is the fresh native Tern orchestrator; it gates safe continuation without losing pending/peer work. Retire it as the active starting point after the new orchestrator has established current revision-bound STATE and explicitly accepted ownership; preserve it as historical evidence rather than silently rewriting its old observations.

— zestroku NTM director (PeachKnoll)

Mission anchor: `7affabf4552ba5a29138cf33fc8e27f2f38ee28076d4e10defecff816f161339`
