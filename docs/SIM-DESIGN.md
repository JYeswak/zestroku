# zestroku-sim design (bead zestroku-m7f-sim-9bt)

Status (2026-10-18): the `zestroku-sim` library, a read-only `two-devices`
scenario CLI, and a consolidated real-binary integration walk exist. The
original bead `zestroku-m7f-sim-9bt` is OPEN; its historical close receipt
`ev_16f0b9eabf22ab2e520577d3266e908cb1ab45c81e21b7cb2872a48ffd101098`
does not establish completion of its current full acceptance. This is the
target design; §9 distinguishes implemented behavior from remaining targets.
Entries marked `FILL` remain unverified wire behavior and must be sampled
read-only into scrubbed fixtures before they can be asserted as product behavior.

Ground truth: `ECP-WIRE-DOC.md` §1–§4 and `CROSS_DEVICE_DIFF.md` in
`skill-library-growth/candidates/edge-library/roku/` (canonical copies live there),
the scrubbed fixtures in that directory's `fixtures/`, and the existing
`Http1Listener` + `HostPolicy::allow_list` server test in
`crates/zestroku-cli/src/transport.rs` (the server template).

## 0. Shape
- Existing library crate `zestroku-sim`; `zestroku sim --scenario two-devices [--json]`
  implements the bounded read-only walk described in §8.
  deps: asupersync (workspace), zestroku-proto (reuse `query`, parsers for round-trip
  asserts), zestroku-types. No tokio, no reqwest, `#![forbid(unsafe_code)]`.
- One `SimDevice` = one `Http1Listener` on `127.0.0.1:0` (ephemeral) + an
  `Arc<Mutex<DeviceState>>`-style state owned by the handler (use a non-std-unwrap
  lock per repo rule; handler is sync over a short critical section).
  Handler signature as in transport.rs test: `|Request| async move { Response }`.
- `HostPolicy::allow_list(["127.0.0.1"])` — the FND-DEPS hazard (default policy 4xxs
  loopback Host headers with a port).
- The sim speaks ECP port-shaped endpoints on ephemeral ports; the CLI already takes
  host+port (Device.port), so no 8060 binding / privileges needed. Endpoint list for
  `devices` = `--no-ssdp --targets` file (written to the test's tempdir) — SSDP is a
  separate UDP responder (§6).

## 1. DeviceState (the state machines)
```
struct DeviceState {
  kind: Express | Tv,            // fingerprint: tv-active-channel only on Tv
  tier: Limited | Permissive,    // live-switchable by scenario step (the M3 flip)
  online: bool,                  // fault: suspend => refuse connections
  latency: Duration,             // fault: per-response delay
  active: Option<AppId>,         // active-app machine
  apps: Vec<App>,                // installed list (Permissive-only query)
  media: MediaState,             // media-player machine
  focus: Focus,                  // keypress machine (Home / InApp / Menu)
  tv_channel: Option<Channel>,   // Tv only
  fw: String, model_number: String, is_tv: bool,   // device-info
}
```
Identifier fields (serial, UDN, MAC, device-id, ad-id, friendly name) are
`FIXTURESERIAL`-class placeholders from fixtures — never copied from live probes.

## 2. Route table (exact, from wire doc §2; tier column = observed live)
| Request | Limited | Permissive | Notes |
|---|---|---|---|
| GET /query/device-info | 200 XML | 200 XML | tier-independent; Server header `Roku/<fw> UPnP/1.0 Roku/<fw>` |
| GET /query/active-app | 200 | 200 | `<active-app><app id=..>Name</app></active-app>`; Home => app id absent form |
| GET /query/apps | 403 | 200 | fixture XML shape (ecp_fixture_permissive_apps.xml) |
| GET /query/icon/<id> | 403 | 200 bytes if installed, 404 if not | 403/404 ambiguity under Limited is REAL: the sim must not leak existence under Limited |
| GET /query/media-player | 403 | 200 | state machine §3 |
| GET /query/tv-active-channel | **404** on Express | 404 on Express | TV: 403 (Limited) -> 200 (Permissive) |
| GET /query/rid, /query/dev-mode | 404 | 404 | absent both tiers |
| GET /keypress/*, /launch/* | 403 (keypress: Limited) / untested | **404** | GET never works: method routing 404 |
| POST /keypress/<key> | 403 | 200, mutates focus | per-op approved in live; sim is free |
| POST /launch/<id> | 403 | 200, sets active, 404 if id not installed | |
Limited 403 body is byte-exact: `ECP command not allowed in Limited mode.` (40 B),
headers `Content-Length: 40`, `Content-Type: text/plain; charset="utf-8"`,
`Cache-Control: no-cache`, Server header. Test vector = the committed
`ecp_fixture_limited_response.txt`; assert sim bytes == fixture body + header set
(Server fw string substituted).
Wrapper-not-filter invariant (finding 1) is a property test: for every route R,
`status_limited(R)==403` implies `status_permissive(R) in {200,404}` — never 403.

## 3. Machines
- **Focus/keypress (Permissive POST):** Home, Back, Select, Fwd/Rev/InstantReplay, Info,
  Search, Enter, Backspace, plus Lit_<char>. Transitions: `Home` => active=None,
  media=Closed. `Select` on Home with highlighted tile => launch. Keep it minimal:
  only transitions the e2e asserts (Home resets; Back from app returns Home when
  stack depth 1; unknown key => 400-class per live behavior, FILL from a live probe
  before asserting).
- **Launch:** POST /launch/<id>: id in apps => active=Some(id), media=Closed, 200;
  else 404. Record `launch_count` for assertions.
- **Media-player:** `Closed -> Play -> Pause -> Play -> Stop -> Close`, driven by
  keypress `Play`/`Fwd`/`Rev`/`InstantReplay` while active != None. XML:
  `<player state="play|pause|close" ...><position>..</position></player>`.
  Position advances by a fake monotonic clock (injected `Clock` trait, test advances
  it) so e2e is deterministic. FILL exact XML from a live Permissive probe
  (232 B Express / 131 B TV per wire doc) — do not invent attributes.
- **Tier flip:** scenario step `SetTier(Permissive)` at runtime; maps to the M3 event.
  Lets one e2e walk Limited -> Permissive and assert the 403->200/404 transition table
  (finding 2: census must be re-walked).
- **TV-only:** `Tv.tv_channel` machine: `Some(Channel)`/`None`; Express routes it 404.

## 4. Faults (target design; current implemented subset in §9)
- `Suspend(device)`: stop accepting (drop listener) => client sees connect refused.
  `Resume`: rebind SAME port if free (else new port = the IP-change scenario).
- `Latency(milliseconds)`: implemented by `SimDevice::set_latency`; the handler
  snapshots the delay under the state lock, then awaits the asupersync timer
  outside that lock. Zero disables the fault; the scenario CLI does not expose it.
- `IpChange`: rebind on a different 127.0.0.x loopback alias (macOS needs `lo0 alias`;
  Linux rch workers accept all of 127/8) — on macOS fall back to port change, documented.
- `Garbage`/`Truncate` response: exercises Malformed paths in proto parsers.
- Assertion hooks the CLI side already has: transport REQUEST_TIMEOUT 5s => keep sim
  latency scenarios < 5s for success, > 5s for the Unreachable path (use a tiny test
  override of the timeout rather than a 6 s sleep).

## 5. Implemented end-to-end walk and remaining matrix

`crates/zestroku-cli/tests/e2e_sim.rs` drives the real binary through explicit-target
discovery, info, apps, key and launch against a Permissive simulator. It writes a
temporary `e2e.jsonl` command/response trace with confirmation arguments redacted.
Additional real-socket coverage remains distributed across `sim_routes.rs`,
`devices_store.rs`, `devices_scan.rs`, `info_tier.rs`, `mutation_cli.rs`,
`installed_apps_api.rs`, `mcp_stdio.rs`, and `serve.rs`. This is not implementation
of every scenario in §8 or a generic scenario-matrix runner.

The integration test implements an opt-in `ZESTROKU_E2E_LIVE=1` and
`ZESTROKU_E2E_TARGET=host:port` branch. That branch performs discovery, info and
apps reads only, without the simulator tier override or automatically minted
approval grants; it returns before key and launch. Source implementation is not
evidence that this exact live branch has been executed. Manual owner-LAN checks
in `docs/LIVE-VERIFY.md` and separate physical-device read receipts are distinct
from executing this test branch. Live mutations remain owner-approved and never
run in CI.

The current live test branch still seeds fixture device and permissive tier rows
in its private database before the read walk. It is not a fresh persisted-tier
discovery/authorization proof and must not be promoted into live mutation proof.

## 6. SSDP e2e boundary (separate from SimDevice)

`zestroku-core::ssdp_io::sweep_to` can target a loopback UDP responder, and `ssdp_sweep_loopback.rs` exercises that discovery seam. `SimDevice` itself serves ECP only; it is not an SSDP responder. Normal discovery still uses the fixed multicast destination, so no simulator-to-full-SSDP end-to-end test exists in this tree.

## 7. Superseded m7f implementation questions and remaining FILL items

The original m7f bead remains open. Test placement is `crates/zestroku-cli/tests`;
`SimDevice` owns state behind `Arc<Mutex<_>>` and recovers poisoned state in
`Shared::lock`. The §2 route and §4 fault table above remains a design target;
only sampled wire facts may be treated as device behavior. `sweep_to(target, mx)`
is independently testable; an SSDP responder inside SimDevice is not implemented.

## 8. Scenario CLI and remaining target matrix

`zestroku sim --scenario two-devices --json` starts an Express and a TV simulator
and emits eight JSONL observations: device-info, apps, active-app and
tv-active-channel for each. The Express channel endpoint expects 404; the TV
expects 200. This uses the real loopback transport directly, not a subprocess
invocation of each named CLI command, daemon-backed discovery, or full SSDP.
No other scenario name in this table is exposed by that command. Except for this
bounded `two-devices` read walk, the table remains a target matrix.

| Scenario | Devices | Steps | What it proves |
|---|---|---|---|
| `two-devices` | express + tv, both Permissive | Implemented CLI: GET device-info, apps, active-app and tv-active-channel on each | Eight loopback observations and expected TV/Express endpoint status; not a complete discovery or daemon workflow |
| `limited-gate` | express, Limited | Serve; `apps`, `media-player`, `key Home` | Exact 40-byte Limited 403 body and headers; `key` refused locally before any POST (`Expect` no POST received) |
| `tier-flip` | express, Limited then Permissive | `apps` (403) -> `SetTier(Permissive)` -> `apps` (200) -> `icon/<absent id>` (404) | Wrapper-not-filter: every Limited 403 becomes 200 or 404, never 403; census must be re-walked after a flip |
| `launch-key` | express, Permissive | `launch <id> --confirm` -> `active-app` -> `key Home --confirm` -> `active-app` | Focus and active-app transitions; launch of an absent id is 404; the action log gets a denied row without `--confirm` and an approved row with it |
| `seeds-only` | express + tv | no SSDP responder; `devices --no-ssdp --targets <file>` | Discovery works with multicast black-holed (m7b box c, offline) |
| `dead-seed` | express + one unbound port | `devices --no-ssdp` with both seeds | A dead seed yields a note and exit 0 while the live device is listed |
| `suspend-resume` | express | Serve -> `Suspend` -> `info` (connect refused, bounded retry) -> `Resume` -> `info` ok | Unreachable-device path and retry (§12.14 pre-work) |
| `ip-change` | express | Serve on port A -> `IpChange` (port B) -> `devices` with SSDP responder | A moved device is found again by the sweep (M1 acceptance) |
| `slow-device` | express | `Latency(d)` below then above the transport timeout | Timeout maps to Unreachable, not a hang; success under the timeout |
| `garbage-body` | express | `Garbage` on `device-info` and `active-app` | Parsers return Malformed; the CLI exits non-zero with a message and no panic |

Each remaining scenario requires a planted negative that breaks its intended
property (for example, wrong 403 body or a POST slipping past the gate). Existing
route negatives do not establish causal coverage for every target scenario.

## 9. As-built status and remaining parity gaps (2026-10-18)

| Capability | Source state | Evidence boundary |
|---|---|---|
| Loopback ECP simulator | Implemented as a library `SimDevice` with a real `Http1Listener`, allow-listed Host, ephemeral port, and request log. | `crates/zestroku-sim/src/lib.rs`, including `SimDevice::start`, `received_requests`, `suspend`, and `resume`. |
| Tier and basic state | Limited/Permissive, app list, active app, key focus, launch, TV-only endpoint, malformed apps and identity placeholders are modeled. | `crates/zestroku-sim/src/lib.rs`; route and planted-negative coverage is in `crates/zestroku-sim/tests/sim_routes.rs`. |
| Fault injection | Suspend/resume, tier flip, per-request latency, malformed apps and unknown identity are implemented. | `set_latency(milliseconds)` uses a reactor timer outside the state lock. Arbitrary truncation/garbage and an IP-change API remain unimplemented. |
| End-to-end tests | `e2e_sim.rs` is a consolidated real-binary discovery/info/apps/key/launch walk with a temporary redacted JSONL trace; other integration files provide additional coverage. | Simulator branches and opt-in live branch have different scopes; no generic execution of the full §8 matrix is claimed. |
| Scenario CLI | `sim --scenario two-devices [--json]` is implemented. | `main.rs` routes to `sim_scenario.rs`; eight direct-transport read observations, not CLI discovery or every planned scenario. |
| Live lane | `e2e_sim.rs` implements the explicit-target live switch for three read commands only. | No live grants or mutations; implementation alone does not prove a successful physical-device execution of that branch. |

The historical m7f close receipt is not fresh evidence for a later revision or
completion of the reopened original. Current published source `3168e55` passed
CI run `39120804853`; the full immutable workspace run RCH650 also passed, with
its exact source overlay recorded in
`docs/evidence/receipts/frontend-policy-consumers-20261018.receipt.json`.
The same CI run executed `latency_fault_delays_but_preserves_exact_responses`:
the real-socket test delays device-info and Limited apps responses, preserves
their contents/status, then clears the fault. This library coverage is not an
executable `slow-device` CLI scenario or proof of every target fault.
Separately, the retrieved macOS binary from `343e325` executed
`sim --scenario two-devices --json` with eight expected observations; that older
artifact is not a native macOS build of `3168e55`. None of these results establishes
all target faults, full SSDP, automated physical-device parity, or a full m7f close.
