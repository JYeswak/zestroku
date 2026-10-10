# zestroku-sim design (bead zestroku-m7f-sim-9bt)

Status: design only; the crate does not exist yet. m7f unblocks when m7b closes.
Entries marked FILL are behaviours not yet sampled from a real device; they must
be sampled read-only (scrubbed into fixtures) before the matching machine is coded.

Ground truth: `ECP-WIRE-DOC.md` §1–§4 and `CROSS_DEVICE_DIFF.md` in
`skill-library-growth/candidates/edge-library/roku/` (canonical copies live there),
the scrubbed fixtures in that directory's `fixtures/`, and the existing
`Http1Listener` + `HostPolicy::allow_list` server test in
`crates/zestroku-cli/src/transport.rs` (the server template).

## 0. Shape
- New crate `zestroku-sim` (lib + `zestroku sim --scenario <name>` subcommand later).
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

## 4. Faults (fault-injection acceptance)
- `Suspend(device)`: stop accepting (drop listener) => client sees connect refused.
  `Resume`: rebind SAME port if free (else new port = the IP-change scenario).
- `Latency(d)`: sleep in handler using asupersync time (no tokio sleep).
- `IpChange`: rebind on a different 127.0.0.x loopback alias (macOS needs `lo0 alias`;
  Linux rch workers accept all of 127/8) — on macOS fall back to port change, documented.
- `Garbage`/`Truncate` response: exercises Malformed paths in proto parsers.
- Assertion hooks the CLI side already has: transport REQUEST_TIMEOUT 5s => keep sim
  latency scenarios < 5s for success, > 5s for the Unreachable path (use a tiny test
  override of the timeout rather than a 6 s sleep).

## 5. e2e suite (rch workers, no LAN)
- Lives in `crates/zestroku-cli/tests/e2e_sim.rs` (or sim/tests) and drives the REAL
  `zestroku` binary via `CARGO_BIN_EXE_zestroku` against sim ports; each command and
  response logged as JSON lines to a tempdir artifact (acceptance wording).
- Matrix (acceptance: devices/info/apps/key/launch): per tier x per device kind.
  Planted negatives: (1) sim Limited returns the wrong 403 body => tier-gate parser test
  red; (2) sim returns 403 under Permissive => wrapper invariant red; (3) `key` without
  confirmation token => refused, no POST observed by the sim (sim exposes
  `received_requests()` so "no POST arrived" is asserted, not inferred).
- `ZESTROKU_E2E_LIVE=1` lane: same test fns, endpoint source switches from sim to
  targets.toml of real devices; skipped (not failed) when unset; documented in
  docs/LIVE-VERIFY.md-style section; never run in CI.

## 6. SSDP responder (for `devices` without --no-ssdp)
UDP responder bound to 127.0.0.1:0 answering M-SEARCH with `ST: roku:ecp`,
`Location: http://127.0.0.1:<port>/`. The CLI's scan target is currently the constant
SSDP_ADDR in ssdp_io — needs an injectable target (env `ZESTROKU_SSDP_ADDR` or a
function arg) before the sim can exercise the SSDP leg; flagged as a core touch
(OrangeShore's ssdp_io) — otherwise e2e covers seeds-only only, and the SSDP socket
keeps its loopback unit test. Also sim serves two other STs (DIAL, rtvaccel) from a
second responder to prove the ST filter end-to-end.

## 7. Open questions for the m7f claimant
1. Crate placement for e2e (sim/tests vs cli/tests): pick cli/tests so
   `CARGO_BIN_EXE_zestroku` is available.
2. Sim lock primitive: repo rules forbid std Mutex unwraps — use parking-free
   pattern (handler owns state behind asupersync sync primitive or Arc<std::sync::Mutex>
   with `lock().map_err`).
3. Unknown real behaviors to PROBE (read-only, Permissive, now doable): key POST error
   codes, media-player XML on both devices, `launch` of absent id. Collect scrubbed
   samples into fixtures/ before coding the machines; machines marked FILL above must
   not ship until sampled.
4. m7f dependency: blocked by m7b close. Sim crate scaffold itself (Cargo.toml,
   workspace member) touches root Cargo.toml + crates/zestroku-sim only; no overlap
   with active reservations.

## 8. Scenario table (`zestroku sim --scenario <name>`)

Each scenario is a list of steps over named virtual devices (`express`, `tv`) that the
e2e suite and a human can both run. A step is `Serve`, `SetTier`, `Suspend`, `Resume`,
`IpChange`, `Latency`, `Garbage`, or `Expect` (an assertion on the sim's request log).

| Scenario | Devices | Steps | What it proves |
|---|---|---|---|
| `two-devices` | express + tv, both Permissive | Serve both; `devices`, `info`, `apps`, `active-app` on each | The §1 read workflows: both devices inventoried, TV-only `tv-active-channel` present on the TV and 404 on the express |
| `limited-gate` | express, Limited | Serve; `apps`, `media-player`, `key Home` | Exact 40-byte Limited 403 body and headers; `key` refused locally before any POST (`Expect` no POST received) |
| `tier-flip` | express, Limited then Permissive | `apps` (403) -> `SetTier(Permissive)` -> `apps` (200) -> `icon/<absent id>` (404) | Wrapper-not-filter: every Limited 403 becomes 200 or 404, never 403; census must be re-walked after a flip |
| `launch-key` | express, Permissive | `launch <id> --confirm` -> `active-app` -> `key Home --confirm` -> `active-app` | Focus and active-app transitions; launch of an absent id is 404; the action log gets a denied row without `--confirm` and an approved row with it |
| `seeds-only` | express + tv | no SSDP responder; `devices --no-ssdp --targets <file>` | Discovery works with multicast black-holed (m7b box c, offline) |
| `dead-seed` | express + one unbound port | `devices --no-ssdp` with both seeds | A dead seed yields a note and exit 0 while the live device is listed |
| `suspend-resume` | express | Serve -> `Suspend` -> `info` (connect refused, bounded retry) -> `Resume` -> `info` ok | Unreachable-device path and retry (§12.14 pre-work) |
| `ip-change` | express | Serve on port A -> `IpChange` (port B) -> `devices` with SSDP responder | A moved device is found again by the sweep (M1 acceptance) |
| `slow-device` | express | `Latency(d)` below then above the transport timeout | Timeout maps to Unreachable, not a hang; success under the timeout |
| `garbage-body` | express | `Garbage` on `device-info` and `active-app` | Parsers return Malformed; the CLI exits non-zero with a message and no panic |

Each scenario has a planted negative recorded next to its test: break the property it
proves (wrong 403 body, a Permissive 403, a POST that slips past the gate) and the
matching test must go red.
