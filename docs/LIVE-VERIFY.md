# Live verification — `zestroku devices` (m7b acceptance (b), (c))

Human-led live legs. Nothing here mutates a device: both commands are
read-only (SSDP M-SEARCH + `GET /query/device-info`). Run from this repo's
canonical checkout on the Mac **on the owner LAN** (192.168.1.0/24 — the rch
workers are off-LAN, which is why this leg needs a local darwin binary).

## Prerequisites (once)

1. darwin binary (local build, Josh-approved bypass or darwin worker):
   ```sh
   cd ~/Developer/zestroku
   nice -n 10 cargo build -p zestroku-cli
   BIN=target/debug/zestroku
   ```
2. Both devices on and on the LAN. Expected ground truth (plan §2):
   - Express 4K+, model **3941X2**, fw **15.2.4** (build 3442), at .182
   - 50RokuTV (L816X / partner 50R4CX), fw **15.3.4**, at .222
   - Both Permissive since 2026-10-10 (M3); irrelevant to these read-only legs.

## Leg 1 — acceptance (b): live SSDP sweep, both devices

```sh
$BIN devices --mx 3
```

**Expected**: two `host:port` lines, exit 0. Values must match plan §2:

```
192.168.1.182:8060	Roku Express 4K+	3941X2	software=15.2.4	tv=false
192.168.1.222:8060	50RokuTV (<model>)	<model-num>	software=15.3.4	tv=true
```

(`model_name` strings come from the devices verbatim — exact text may differ
slightly from the plan's phrasing; the load-bearing fields are model number,
software version, `tv=` flag, and BOTH devices present.)

**Failure modes**: one device missing → rerun once (SSDP is lossy; checklist
§1: repeat the sweep). Still missing → note which, check the device's power
state, run Leg 2 to isolate SSDP vs ECP. Empty output + exit 1 → multicast
black-holed; Leg 2 is exactly the designed fallback.

## Leg 2 — acceptance (c): seeds-only run (SSDP black-holed by construction)

`--no-ssdp` skips the socket entirely (see `devices.rs`); the seeds file must
live OUTSIDE the repo (identifiers never land in git — `scripts/check-identifiers.sh`).

```sh
SEEDS="$TMPDIR/zestroku-live-targets.toml"   # use your owned scratch dir
cat > "$SEEDS" <<'EOF'
[[target]]
host = "192.168.1.182"
[[target]]
host = "192.168.1.222"
EOF

$BIN devices --no-ssdp --targets "$SEEDS"
```

**Expected**: same two lines as Leg 1, exit 0 — proving the seeds-only path
inventories both devices with SSDP unreachable (the frankensonos
flaky-multicast escape hatch, FS:113-114).

Negative control (optional, 10 s): point `--targets` at a file with one bogus
host (e.g. `192.0.2.99`); expect `note: 192.0.2.99:8060: …` on stderr, that
host absent from stdout, exit still 0 if a real device answered — the run
survives a dead seed (m7d stores last-known state).

## Rollback / cleanup

No device state touched; nothing to roll back. `rm "$SEEDS"` when done.
Do NOT commit the seeds file or paste raw device-info output (serial/UDN/MAC
scrub rules apply) — record only the fields compared above.

## After both legs

Post `DONE m7b (b)(c)` to zestroku-guide-loop with the two output blocks
(model/fw/tv fields only) + exit codes. The bead closer (director) runs the
independent offline re-run; live legs are Josh's evidence, not re-runnable in CI.
