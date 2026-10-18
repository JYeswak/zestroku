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

## Mission

Make zestroku a complete, installed, resident control system that Joshua can use
from his Mac terminal, local agents, authorized tailnet agents, HTTP clients,
and browser/phone remote to discover, inspect, and safely control his own
supported Roku devices. Every required capability has an explicit positive
result, negative/boundary behavior, applicable device/tier matrix, and
independently witnessed current-source acceptance. First-run setup, persistence,
recovery, diagnostics, policy, audit, and truthful partial failure are part of
usability, not later ecosystem hardening. Per-operation owner approval remains
mandatory for device mutations. Simulator proof never substitutes for
physical-device or real-tailnet proof.

Approved by Joshua on 2026-10-15: "yes I approve this", in response to the
surface and negative-acceptance proposal; approval recorded in Agent Mail
AM53952. October 18, 2026 is the selected target, not evidence of completion.
The requirements below describe the approved destination, not shipped behavior.

### Required surfaces

| Surface | Positive acceptance | Negative acceptance |
|---|---|---|
| Installed Mac CLI | Stable command path, source-bound version, root/subcommand help, JSON and completions from a fresh shell. | Wrong-platform/stale/dirty artifacts are not current proof; invalid input fails; diagnostics never contaminate JSON. |
| First-run setup | Empty-store setup reaches discovery and a usable first read, explaining required owner actions. | No developer-seeded DB as first-run proof; no silent permission, tier, or device-state changes. |
| Resident macOS service | Supported launchd deployment in a LAN-permitted context; persistence, restart and crash recovery. | Empty healthy daemon is not readiness; duplicate instance, occupied port, invalid config and store errors fail explicitly. |
| Local HTTP API | Documented operations reach shared core and authoritative inventory; OpenAPI matches execution. | Unknown target, invalid body, approval refusal and device failure remain distinct; no placeholder success. |
| MCP stdio | Real agent initialize, tool/resource discovery, reads and approved control through daemon. | Listing/echo is not execution; daemon failure never falls back to LAN; approval-only text is not mutation success. |
| MCP streamable HTTP | Real client exercises supported capabilities with stdio-equivalent semantics. | No weaker approval/policy/target rules; cancellation or disconnect must not duplicate mutations. |
| Tailnet access | Authorized off-LAN client reaches daemon and completes intended workflow; devices remain LAN-only. | Public/wildcard exposure and Funnel refused under deployment contract; unknown principal never becomes owner. |
| SSE | Real subscribers observe state/reachability deltas with documented reconnect behavior. | No fabricated unchanged-state events, silent stale state, or unbounded slow-client buffering. |
| Browser/phone remote | Real devices/current state and owner-approved controls on intended local/tailnet browser. | Foreign Host/Origin and invalid write types rejected before effects; no tokens in URLs or premature success. |
| Configuration/store | Documented seeds, aliases, defaults, policy, tiers, inventory, caches and history survive intended restarts. | Invalid config is actionable; migrations preserve data; interrupted identity moves do not lose associations. |
| Diagnostics/operator controls | Doctor, service status, logs and errors identify failure and remedy. | Doctor never mutates devices; failed prerequisites skip honestly; no false health or secrets in evidence. |
| Runnable simulator | `zestroku sim --scenario two-devices` exercises documented commands with real sockets/store. | Explicit simulator classification; no household traffic/credentials; injected faults exercise actual consumers. |

### Required capabilities and negative controls

Each applicable CLI, HTTP, MCP and browser entry point must reach the same
authoritative implementation and policy. Human approval minting remains
interactive-owner-only; it must not be exposed as a remote approval endpoint.

| Capability | Positive acceptance | Negative/boundary acceptance |
|---|---|---|
| Discovery/inventory | Both owner devices persisted through supported SSDP, seeds and explicit scans; report mechanism. | Off-scope SSDP probes refused before I/O; duplicates reconciled; seed success is not multicast proof. |
| Identity/aliases/defaults | Stable identity, aliases/class targets and per-client defaults follow address changes. | Distinct devices never merge; ambiguity/typos never select mutation targets; defaults do not redirect explicit targets. |
| Information/capabilities/tier | Current firmware/class/capabilities and observed tier with freshness/provenance. | Unknown is not Permissive or unsupported; optional absent fields remain distinguishable. |
| Installed apps/search | Actual app IDs/names, safe selection and useful suggestions. | Missing/ambiguous apps cause zero mutation; never guess IDs or override daemon authority with stale client state. |
| Active app/media/TV state | Typed supported state with freshness and device-class applicability. | Distinguish tier refusal, unsupported/absent resource, malformed response and unreachable device. |
| Keys/control | Approved supported action reaches intended device exactly once with completed audit. | Wrong device/action, invalid/expired/replayed token, tier/policy refusal and invalid key cause zero mutation; preserve case-sensitive text semantics. |
| Launch/input/deep links | Selected installed app and explicitly supported input/deep-link behavior. | No uncertain fuzzy launch, backend override, silent fan-out or invented support; bind approvals to actual action. |
| Developer sideload | Owner archive/credentials use actual installer endpoint with meaningful result verification. | Wrong approval/port, invalid archive, Digest failure and HTTP-200 error pages fail; no secret/archive leakage. |
| Action history | Completed/denied actions expose target, principal/surface, intent, before-state and truthful result. | Approval is not completion; device/transport/audit-finalization failure cannot masquerade as fully recorded success. |
| Undo | Fresh approved restoration of supported prior state, itself audited. | Never undo denied/pending history; missing prior app/device or unrestorable focus reported honestly. |
| Policy/principals | Allowlist keys are `cli`, `mcp-stdio`, and `loopback-http`; each approval grant is bound to one key. | No surface bypass; unknown or mismatched principal is read-only; denials dispatch zero and preserve unused approval. |
| Cache/poll/recovery | Documented warm-read latency/freshness; bounded suspend/address-change recovery and backoff. | Label stale data; no blocking executor network work or polling storms; bounded read retry only, never automatic mutation retries. |
| Errors/machine output | Stable semantic codes, hints, suggestions and structured results across surfaces. | No silent error or generic success on partial failure; exit classes may represent multiple semantic codes. |

The daemon's first poll begins after the listener is bound; successful
snapshots are refreshed on a 30-second per-device interval. A snapshot is
fresh through 60 seconds. Per-device failures retain the last snapshot and
back off exponentially from 5 to 300 seconds.
`get_device_state` and `roku://devices/{id}` include `fresh` and `observed_at`;
without any snapshot they report `DEVICE_UNREACHABLE`. `info`, `active_app`,
and `apps` serve fresh inventory data and otherwise retain their live-read
behavior.
The snapshot omits serial number, UDN, and MAC. MCP lists `roku://devices`
with `resources/list` and `roku://devices/{id}` with
`resources/templates/list`.

For keys, input/deep links, icons and device-specific capabilities, record
required, device-dependent, deferred or unsupported explicitly. Do not infer
support from a protocol name or silently lose it between documentation and
implementation.

Acceptance evidence names the entry point, device/tier, setup, positive result,
negative case, side-effect count, timing/freshness bound, source/artifact,
independent verifier and disposition. Real-device mutations require fresh
point-of-use approval; an unavailable device leaves its live acceptance open.

### Scope decisions requiring reconciliation

- Default clients use the authoritative daemon without silent direct fallback;
  retain explicit direct diagnosis, not the old automatic-fallback recipe.
- Define permitted listeners per deployment mode; historical owner-LAN,
  loopback/tailnet and unsafe-bind descriptions are not interchangeable.
- Resident product operation supersedes the old on-demand-only exclusion;
  agent watchers remain separate.
- Roku has no Sonos topology/UPnP-800 recovery path. Preserve Roku-specific
  address-change, suspend and tier-change recovery instead.
- State what can actually be snapshotted/restored; do not promise unobservable
  focus restoration.
- Enumerate intended parity rather than exposing human approval minting or
  service administration as agent tools.
- Scenes/schedules remain owner-gated deferred; DJ, feedback, grouping,
  playback transfer and announcements have no accepted Roku analogue.
- Ecosystem tracker releases and skill admissions remain backlog obligations,
  not substitutes for or blanket prerequisites to local product capability.

## Build and run on the owner's Mac

Preferred route (no rch worker needed): `gh workflow run darwin.yml --ref main`
builds a native Mach-O arm64 binary on a GitHub macOS runner, prints its
`file`, `shasum -a 256` and `--version` in the run log, uploads it as the
`zestroku-aarch64-apple-darwin` artifact (`gh run download <run-id> -n
zestroku-aarch64-apple-darwin`) and runs the offline tests natively. Clear the
download quarantine with `xattr -d com.apple.quarantine zestroku`. The Contabo
cross-build below is the fallback.

`portable-macos.yml` runs a stock Rust build, the freshly built binary's
`sim --scenario two-devices --json` read smoke, and full locked workspace tests.
The smoke uses a Cargo-only PATH without RCH or omp-kit and is explicitly
simulated, not a mission verdict.

The public job no longer runs the private owner-mission invocation: its
registration, artifact declarations, standalone approval/denial/inverse probes,
local-Cargo mission-executor integration and live-device coverage are not
replaced by this smoke. Rust safety regressions remain in the workspace suite.
Bun 1.4.3 remains the runtime for checker tests and the four owner-mission
commands; their strict UNKNOWN handling and live acceptance bars are unchanged.
This does not grant local Rust compilation; use the executor approved for the host.

For the optional Contabo cross-build, select Darwin through Cargo's
`build.target` configuration; do not use RCH's `--target` OS selector.
`ring` compiles C, so this route uses `bin/zigcc-darwin-adapter.sh` and the
fleet's Zig/SDK shim. The wiring is opt-in per invocation (2oz.1), never a
committed target default: a stock Apple-Silicon checkout uses system clang.
The cross-build is a separate producer from native macOS; verify its own
artifact identity rather than assuming byte-identical binaries.
From a saved commit C:

```
C="$(git rev-parse HEAD)"
env -u RCH_WORKER RCH_REQUIRE_REMOTE=1 rch exec \
  --base "$C" --clean-overlay --no-overlay -- \
  cargo build --release -j 2 --locked \
  --config 'build.target="aarch64-apple-darwin"' \
  --config "env.ZESTROKU_BUILD_SHA.value=\"$C\"" \
  --config 'env.ZESTROKU_BUILD_SHA.force=true' \
  --config 'env.ZESTROKU_BUILD_DIRTY.value="false"' \
  --config 'env.ZESTROKU_BUILD_DIRTY.force=true' \
  --config 'env.CC_aarch64_apple_darwin.value="bin/zigcc-darwin-adapter.sh"' \
  --config 'env.CC_aarch64_apple_darwin.relative=true' \
  --config 'env.CC_aarch64_apple_darwin.force=true' \
  --config 'target.aarch64-apple-darwin.linker="bin/zigcc-darwin-adapter.sh"' \
  -p zestroku-cli
```

Local Cargo output (`target/`, `target-local2/`, `t-local/`) and native tracker/NTM
runtime caches are ignored, not source or publishable evidence. WAL recovery
journals remain on disk; durable acceptance receipts belong in `docs/evidence/receipts/`.

Use a bounded job and require RCH's remote completion plus the retrieved
`target/aarch64-apple-darwin/release/zestroku` artifact. `file` must report
`Mach-O 64-bit executable arm64`. An untargeted artifact is a Linux ELF and is not
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
  'exec env ZESTROKU_TRANSPORT=std "$HOME/Developer/zestroku/target/aarch64-apple-darwin/release/zestroku" serve --bind 127.0.0.1:8070'
```

Discovery and the daemon share `ZESTROKU_TRANSPORT`: `std` selects the existing
macOS LAN workaround; unset or `asupersync` selects the normal client. Unknown
values produce a diagnostic and deterministically use asupersync. The selector
does not grant LAN permission: run explicit `ZESTROKU_DIRECT=1` discovery in
the permitted process tree, using the same `ZESTROKU_DB` as the daemon,
to populate its inventory before starting it.

For MCP and default CLI `key`/`launch`, `ZESTROKU_DAEMON_URL` defaults to
`http://127.0.0.1:8070` and must be `http://<loopback-ip>:<nonzero-port>`.
Invalid endpoints are rejected before mutation dispatch. `ZESTROKU_DIRECT=1`
selects diagnostic direct ECP and requires LAN permission in the CLI's own tree.

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

`zestroku devices` lists the daemon's cached inventory without discovering or
probing devices. `--json` emits `source: "daemon"`, a `devices` array with
`name`, `host`, `model`, `model_number`, `software_version`, `access_tier`,
and `last_seen`, and an empty `notes` array. This cache view does not provide
an ECP port or TV classification; neither value is fabricated.

Use `ZESTROKU_DIRECT=1 zestroku devices` for explicit discovery and inventory
refresh (SSDP plus optional seeds). Discovery flags require this mode.
macOS gates UDP multicast per executable, so use `--targets <file>` with
`--no-ssdp`, or `--scan <cidr>` (private ranges, at most a /22), when needed.
Direct discovery retains its device-info JSON fields, including `port` and
`is_tv`, plus structured per-target failure notes.
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
Like `info`, it uses the configured daemon by default and reports
`DAEMON_UNAVAILABLE` instead of silently falling back to direct ECP.
`ZESTROKU_DIRECT=1` retains explicit direct access for owner-led diagnosis.
App names and versions are not stable across firmware.
`zestroku alias set <alias> <device>`, `zestroku alias rm <alias>`, and
`zestroku alias ls` manage local device labels. Host arguments accept exact
aliases, a unique `tv` or `stick` class target, or a unique device-name prefix.
Ambiguous matches list candidates; mutations never select one implicitly.
`zestroku approve <launch|key> <host> <target> [--client <cli|mcp-stdio|loopback-http>]` requires an interactive owner TTY and
a typed action phrase. It prints a random, five-minute, single-use token bound
to the selected client, device endpoint, and command; the default client is
`cli`. Use `--client mcp-stdio` or `--client loopback-http` for those consumers.
Then pass the token to `zestroku key <host> <KEY> --confirm <token>` or
`zestroku launch <host> <app-id> --confirm <token>`.

Daemon mutation requests default `client` to `loopback-http`; the CLI and MCP
adapters send their own expected labels. The daemon accepts a label only when
it matches the client bound to the owner-issued token; unknown or mismatched
clients cannot dispatch.

If `policy.toml` has an `[allow]` table, use these exact client keys; legacy
`http` and `mcp` names are not aliases and do not inherit permissions.
Tokens issued before client binding remain unresolved and are refused; reissue them.
Default CLI `key` and `launch` forward the opaque token to the daemon, which
owns the before-state snapshot, recorded-tier/approval gate, command dispatch
and final audit outcome. The owner `approve`
command must use the same `ZESTROKU_DB` as the daemon. A down daemon fails
without consuming a local token or falling back to ECP. `ZESTROKU_DIRECT=1`
retains the local diagnostic mutation path.

`zestroku undo <host> --confirm <token>` restores a supported prior app through
the daemon. Eligible history includes approved, completed CLI, MCP stdio and
HTTP actions for the same device; older direct-mode success records remain
readable. Restoration needs its own fresh approval. Denied or pending actions
are not eligible, and a missing or uninstalled prior app refuses restoration
before dispatch.

`zestroku launch <host> --app <name> --confirm <token>` resolves an ASCII
case-insensitive exact name, then a unique prefix, against the daemon's
installed apps. Missing or ambiguous names never dispatch. The approval
still binds the numeric app ID; the audit records the requested label and
resolved ID before dispatch. The label is caller metadata, not an authority
or a substitute for the command-bound approval.
`zestroku approve install <host> sideload [--installer-port <port>] [--client <cli|mcp-stdio|loopback-http>]` defaults to port 80 and `cli`.
Only the CLI install path currently consumes this grant; it binds to the exact installer port and `POST /plugin_install`.
`zestroku install <host> <zip> --password-stdin [--installer-port <port>] [--confirm <token>]`
uploads the owner-provided ZIP to the Roku developer installer using Digest auth.
The username defaults to `rokudev`; override it with `--username`. Supply the owner-set
developer password through stdin, not an argument. ZIP contents and credentials are
never stored in the repository or mutation audit record. The response body is checked
for Roku HTML error markers; HTTP 200 alone is not treated as success.
Blank, arbitrary, mismatched, expired, and reused tokens are refused
(exit 4) and audited by the daemon for default `key`/`launch`; direct
diagnosis and `install` retain their local gates. HTTP and MCP key/launch
requests use the same opaque token; neither surface can mint approvals.
`zestroku serve [--bind <ip:port>]` exposes `GET /health`,
`GET /v1/devices`, `GET /v1/devices/{host}/info`,
`GET /v1/devices/{host}/active-app`, `GET /v1/devices/{host}/apps`, and the
confirmation-gated `POST /v1/devices/{host}/key` and
`POST /v1/devices/{host}/launch` routes over the shared store on loopback by
default (`127.0.0.1:8070`).
It binds loopback or one of this node's Tailscale addresses only, and the
Host header must match the bind address. A tailnet bind checks local
`tailscale status --json` on the daemon runtime before opening the store or
listener. One five-second deadline covers both command completion and stdout
EOF, including an inherited writer; refusal closes the pipe and kills/reaps
the owned command without leaving a reader thread. Loopback startup does
not require Tailscale.
The info response includes the observed `access_tier` and `tv_active_channel`;
a fresh inventory snapshot may satisfy it without another ECP query. The
channel field is `null` when the optional ECP probe fails.
The daemon prints its listening address only after binding succeeds; an
occupied port exits unsuccessfully without claiming readiness. SIGINT
(Ctrl-C) and SIGTERM both stop admission and drain the running HTTP server.
The offline shutdown regression keeps its target known in inventory before
admission and checks an uncached ECP read, complete response framing and EOF,
and listener release—not merely that the process stopped.
`zestroku --version` prints `zestroku <semver> <git-sha7>[-dirty]`.

The store refuses a database whose applied schema version is newer than the
running binary supports. Use a compatible newer binary, or restore a validated
compatible backup (below); never delete migration history to force an older
binary to open it.

### Upgrade, backup and rollback (macOS)

`scripts/install-macos.sh` installs a source-bound binary at
`$PREFIX/bin/zestroku` (default `~/.local`) and preserves the previous one as
`zestroku.prev`:

```
scripts/install-macos.sh --artifact ./zestroku --sha256 <digest> \
  --version <semver> --sha7 <sha7> --data-backup ~/zestroku-pre-upgrade.db
scripts/install-macos.sh --rollback --restore-data ~/zestroku-pre-upgrade.db
```

The install refuses, with exit 2 and nothing replaced, a digest mismatch, a
file that is not an arm64 Mach-O, a candidate whose `--version` is not exactly
`zestroku <semver> <sha7>`, and a failed data backup. `--data-backup` runs
`zestroku db backup <file>` from the verified candidate before anything
replaces or migrates the store: the snapshot keeps the schema the data is at,
and an existing file is never overwritten.

`zestroku db restore <file>` is offline. It validates the backup (SQLite
header, a schema ledger the binary supports, so a newer-schema backup is
refused), checkpoints the live store and requires a complete checkpoint
status, then promotes a synced copy by rename. It refuses while a writer holds
the store and never deletes WAL frames it cannot consolidate.

Rollback also asks the PRIOR binary to open the data it will inherit (the
backup, or a snapshot of the live store for a binary-only rollback) on a
scratch copy before anything is restored or promoted: data too new for the
prior binary is refused with binary and data untouched.

The script never stops or starts the resident daemon. After a failed upgrade:
stop the daemon through your approved launchd route, run the rollback command
above (the newer binary restores the data first; a refused restore leaves both
binary and data untouched, and an interrupted restore leaves the live store
unchanged beside a `.restore.tmp` staging file, so retry or keep the backup),
then restart the daemon. `ZESTROKU_DB` selects the store for both verbs.
`scripts/test-install-macos.sh <old> <new>` runs the consumer scenario over
two real builds; the manual `darwin` workflow runs it when given `old_ref`.

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

## Policy configuration

`policy.toml` lives beside the store database. A missing file uses the
documented budgets: eight keypresses per second and ten launches per minute.
The supported root fields are `keypress_per_sec`, `launch_per_min`, `allow`,
and `quiet`. Both budgets are positive integers; `allow` maps client names to
arrays containing `keypress`, `launch`, and/or `install`.

`quiet` accepts only `start` and `end`, UTC hours from 0 through 23. The start
is inclusive and the end exclusive; windows may wrap midnight. Equal hours
disable the window. Unknown fields or tables are errors, not ignored
restrictions: `[quiet_hours]` is not an alias for `[quiet]`, and extra fields
such as `quiet.enabled` are refused. Doctor reports invalid policy rather
than endorsing a silently unused restriction.

Policy code is not yet qualified as a complete cross-surface safety contract;
trusted client identity and durable concurrent audit corrections remain open.

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
- Doctor and policy code are present, but their original cross-surface and
  causal acceptance remains incomplete. SSE state stream and web remote are
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
