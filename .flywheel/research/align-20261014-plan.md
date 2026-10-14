# Zestroku Current CI and Device Evidence Plan

LANG:EN. Task: align-20261014. Phase: PLAN, paper only. Owner: PeachKnoll; chief: RedMaple. No execution is authorized by this document.

## Goal and authority

KNOW: The mission is one owner-LAN Roku CLI/daemon binary (`.flywheel/MISSION.md:3`), with per-operation launch/install approval, no flashing, and no committed device identifiers or captures (`.flywheel/MISSION.md:5`). Completion requires all origin/main CI gates green, independent M7 acceptance reruns, and both owner devices discovered and stored on Darwin (`.flywheel/GOAL.md:5`).

KNOW: `.flywheel/STATE.md:1` dates the state to October 11. Its red-CI revision (`:4`), landed inventory (`:5`), frontier (`:6`), and divergence (`:7`) are historical reports, not current evidence. Chief accepted RESEARCH warn and required current CI/device evidence before further planning decisions.

INFER: CI evidence and live device evidence must bind to the same source revision; a green unrelated revision or an old binary cannot establish current completion. Current CI status, live reachability, stored inventory, and available binary provenance remain UNVERIFIED. No GUESS claims are used.

## Scope and restrictions

Collect evidence only after the chief authorizes execution. Do not pull or claim beads, restore sessions, start watchers, implement fixes, mutate devices, or change operating protocol. Do not rebase, stash, alter another worker's files, or treat old conversation evidence as current proof. This file is the only planned project edit in this tick.

## 1. Bind CI evidence to current main

After authorization, use read-only GitHub queries:

```sh
gh repo view --json nameWithOwner,url
gh api repos/{owner}/{repo}/commits/main --jq .sha
gh run list --branch main --workflow ci.yml --limit 10 --json databaseId,headSha,status,conclusion,url,createdAt,updatedAt
gh run view <matching-run-id> --json headSha,status,conclusion,url,jobs
gh run view <matching-run-id> --log
```

Replace owner/repo from the first result and select a run whose full head SHA equals current remote main. Capture complete output and exit codes before reducing it to a summary. Do not launch a new workflow during this paper-only phase. A missing, queued, skipped, cancelled, or unsuccessful gate is not green.

KNOW: `.github/workflows/ci.yml:42-74` defines fmt, check, clippy, offline test, one-asupersync, tokio/reqwest ban, and identifier scrub. Record each gate's observed conclusion, run URL, source SHA, and collection time. A workflow conclusion alone is insufficient if a required step was skipped. Re-read workflow at the selected revision to avoid comparing old runs with today's gate definitions.

Negative check: reject any successful run whose SHA differs from main. If main moves during collection, label the collected revision explicitly and re-evaluate which revision is current; do not silently promote stale evidence.

## 2. Establish a provenance-bound Darwin binary

KNOW: `README.md:17-23` documents the GitHub Darwin artifact route; `README.md:47-57` requires Mach-O arm64 and a matching, non-dirty version SHA. Remote builds are required by repo doctrine; do not use the local build suggestion in `docs/LIVE-VERIFY.md:23-28` without explicit owner authorization.

After authorization, inspect an existing successful `darwin.yml` run for the same full revision and its artifact/log provenance. Use `gh run list --branch main --workflow darwin.yml` and `gh run view <run-id> --log`. Download only after provisioning owned repo scratch with `.owner` and TMPDIR according to machine rules. Record the artifact SHA-256, source full SHA, `file <binary>` result, and `<binary> --version` output/exit code.

Reject Linux ELF, wrong-revision, dirty, or provenance-unknown binaries. If no matching artifact exists, report that exact prerequisite to the chief; do not start a build or dispatch a workflow without authorization. Existing remote fallback is in `README.md:32-40`; any subsequent build must respect rch, load, storage, and build-slot requirements.

## 3. Verify both live devices, without mutation

KNOW: `docs/LIVE-VERIFY.md:3-6` describes owner-LAN read-only discovery; `:34-75` defines SSDP and seeds-only legs. `README.md:100-111` explains multicast restrictions and explicit direct mode. The documented October device versions (`docs/LIVE-VERIFY.md:29-32`) are comparison history, not requirements that current firmware remain unchanged.

After authorization, confirm the Mac is on the owner's LAN and both owner devices are powered. Use the revision-bound binary and isolated, owned scratch for a fresh verification store and seeds. First inspect the current binary's `--help`, `devices --help`, and `serve --help` to establish its exact store selection and read commands; do not guess an undocumented store flag. Determine the store configuration from current source before any live invocation if help is insufficient.

Run:

```sh
<binary> devices --mx 3 --json
<binary> devices --no-ssdp --targets <owned-seeds-file> --json
```

Configure the isolated store through the verified supported mechanism for both commands. Seeds contain only the two owner-approved endpoints and remain uncommitted. Record complete outputs and exit codes in owned scratch, then retain only scrubbed decisive fields in durable evidence. Confirm two distinct expected owner devices by model/class and endpoint mapping privately; publish model, software, TV classification, count, and leg result without serial/UDN/MAC/ad-id or raw device-info.

If SSDP misses a device, follow the existing bounded retry/fallback procedure (`docs/LIVE-VERIFY.md:51-54`); a successful seed leg does not retroactively prove SSDP. Do not change OS permissions, device settings, or approval policy to pass the check.

## 4. Prove storage independently of discovery output

INFER: Successful discovery output alone does not prove persistence. `README.md:100-101` promises discovery records devices, and `README.md:137-142` documents daemon inventory at `GET /v1/devices` over the shared store.

After finding the exact store configuration, stop the owned discovery process, start the same revision-bound daemon against that store on a free loopback-only port, and read `/health` and `/v1/devices`. Record HTTP status, complete response, and process exit/lifecycle facts. Confirm both discovered devices are returned after reopening the store, not merely present in a process-local cache. Stop only the process launched and recorded for this verification.

Negative check: query the daemon using a separate empty verification store; it must not inherit the prior inventory. Do not clear the owner's real store or modify unrelated daemon instances. Keep endpoint-level comparisons private and publish scrubbed results.

## 5. Independent review and durable evidence

KNOW: Independent acceptance is required by `.flywheel/GOAL.md:5`; raw identifiers must not be committed (`.flywheel/MISSION.md:5`). Use the existing receipt tooling and `docs/evidence/receipts/` convention, not a new receipt implementation (`README.md:43-45`). Inspect the existing receipt contract before signing; bind source/binary digests and decisive scrubbed outputs, and verify the resulting receipt. A receipt id is not invented in advance.

After chief authorization, a non-author reviews each acceptance item and its revision/artifact binding. Record KNOW for directly observed outcomes, INFER for deductions, and GUESS only for explicitly unverified hypotheses. Distinguish offline CI, artifact provenance, SSDP, seed fallback, and persistence; do not inflate one into another. No bead closure is part of this plan.

## Acceptance and decision

This evidence collection is complete only when:

- Current remote main's full SHA and matching CI run are recorded; every required gate is observed successful, or the exact failing/missing gate is reported.
- A matching clean Darwin arm64 binary has observed format, version, and SHA-256 provenance, or its precise missing prerequisite is reported.
- Both owner devices have fresh read-only discovery evidence with separate SSDP and seed-leg outcomes.
- Both devices survive an isolated-store reopen and appear in daemon inventory; the empty-store negative does not inherit them.
- Durable evidence is scrubbed, receipt verification is observed where required, and independent review is identified, not fabricated.

INFER: Only then can the chief choose the next paper PLAN against current gaps. Failed checks stay visible; no gate weakening, mutation, automatic implementation, or watcher startup follows from this document.

## Delivery and check-in

Write this requested file, verify it is readable, and send the chief its exact path through Agent Mail plus `herdr pane send-text w4:p1` followed by `herdr pane send-keys w4:p1 Enter`. Until the file exists, report progress every ten minutes; file creation ends that temporary check-in requirement. Watchers remain off. Execution awaits the chief's decision.
