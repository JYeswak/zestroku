#!/bin/sh
# Consumer scenario for scripts/install-macos.sh (1r4.9): two real source-bound
# Mach-O builds, a populated real Store and the real `zestroku` verbs.
# Fresh-shell usage, on a Mac, as a non-root user:
#
#   scripts/test-install-macos.sh <old-binary> <new-binary>
#
# The two binaries must be builds of two REAL saved revisions, each carrying
# its own `zestroku <semver> <sha7>` identity (the sha7 of the commit it was
# built from), and neither may be -dirty. Never relabel one source tree with
# another revision's identity: that is false provenance. The old revision
# may predate the `db` verb; rollback therefore restores data with the NEW
# binary first. Scratch goes under $TMPDIR (agents: set it to the repo's
# var/agent-tmp dir first); it is left for the scratch reaper. Every command's
# stdout, stderr and exit code is retained under $WORK/evidence.
#
# Domain data: the store is seeded with inventory (devices), aliases, a
# completed and a denied history row and the full schema ledger. Readback
# before the upgrade and after the rollback uses the real CLI (alias ls, log
# --json) plus read-only sqlite3 queries for the device rows and the ledger;
# stock sqlite3 seeds fixtures and reads, it is never the backup or restore
# client.
#
# Scenarios: S1 fresh install; S2 refusals (wrong digest, wrong platform,
# stale/provenance input, failed data backup) each leaving the working binary
# byte-identical; S3 upgrade with a pre-upgrade backup, then S3b a
# deterministic FAILED MIGRATION of the candidate (ledger rows >= 7 removed
# while their tables remain, so migration 7 cannot apply); S4 a non-database
# restore source (refused by the prior-binary probe first); S4b/S4c the
# destination-version gate (a backup, or the live store, too new for the
# PRIOR binary is refused by that binary before anything is restored or
# promoted); S4d a valid SQLite file with no schema ledger, which passes the
# probe and is refused by `db restore` itself; S4e a restore that cannot
# complete (read-only state directory) leaves binary and data untouched and
# succeeds on retry; S5 failed-candidate rollback restoring the pre-upgrade
# data with domain readback equal to the pre-upgrade readback.
set -eu

[ $# -eq 2 ] || { echo "usage: $0 <old-binary> <new-binary>" >&2; exit 2; }
OLD=$1
NEW=$2
HERE=$(cd "$(dirname "$0")" && pwd)
INSTALL="$HERE/install-macos.sh"

fail() { echo "SCENARIO FAIL: $1" >&2; exit 1; }
sha() { shasum -a 256 "$1" | awk '{print $1}'; }
ver() { "$1" --version; }
semver_of() { ver "$1" | awk '{print $2}'; }
sha7_of() { ver "$1" | awk '{print $3}'; }

[ "$(id -u)" -ne 0 ] || fail "run unprivileged: the interruption scenario relies on directory permissions"
[ -x "$OLD" ] && [ -x "$NEW" ] || fail "both binaries must be executable files"
[ "$(ver "$OLD")" != "$(ver "$NEW")" ] || fail "old and new must carry distinct identities"
case "$(ver "$OLD") $(ver "$NEW")" in
  *-dirty*) fail "a -dirty build is never acceptable evidence" ;;
esac
command -v sqlite3 >/dev/null || fail "sqlite3 is required to seed fixtures and read rows"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/zestroku-install.XXXXXX")
EVID="$WORK/evidence"
mkdir -p "$EVID"
STEP=0
# Run a command, retaining stdout/stderr/exit code per step as evidence, and
# keep the latest in $WORK/out, $WORK/err and $RC without tripping `set -e`.
run() {
  label=$1; shift
  STEP=$((STEP + 1))
  n=$(printf '%02d' "$STEP")
  set +e
  "$@" >"$WORK/out" 2>"$WORK/err"
  RC=$?
  set -e
  cp "$WORK/out" "$EVID/$n-$label.out"
  cp "$WORK/err" "$EVID/$n-$label.err"
  echo "$RC" >"$EVID/$n-$label.rc"
  echo "$n $label rc=$RC" >>"$EVID/index.txt"
}

PREFIX="$WORK/prefix"
BIN="$PREFIX/bin/zestroku"
PREV="$BIN.prev"
export HOME="$WORK/home"
mkdir -p "$HOME" "$WORK/state"
export ZESTROKU_DB="$WORK/state/actions.db"
OLD_VER=$(semver_of "$OLD"); OLD_SHA7=$(sha7_of "$OLD")
NEW_VER=$(semver_of "$NEW"); NEW_SHA7=$(sha7_of "$NEW")
OLD_SHA=$(sha "$OLD"); NEW_SHA=$(sha "$NEW")
echo "scratch: $WORK"
echo "old: $(ver "$OLD") sha256=$OLD_SHA"
echo "new: $(ver "$NEW") sha256=$NEW_SHA"

untouched() { # the working binary is still the old build and nothing was staged
  [ "$(sha "$BIN")" = "$OLD_SHA" ] || fail "$1: working binary changed"
  [ ! -e "$PREFIX/bin/.zestroku.new" ] || fail "$1: staged candidate left behind"
  [ ! -e "$PREV" ] || fail "$1: a prior-version slot appeared"
}
still_upgraded() { # NEW is installed and OLD is preserved byte-for-byte
  [ "$(sha "$BIN")" = "$NEW_SHA" ] || fail "$1: installed binary changed"
  [ "$(sha "$PREV")" = "$OLD_SHA" ] || fail "$1: prior slot changed"
}
# Read-only digest of the live store that works even when the store cannot be
# opened by the installed binary (stock sqlite3, SELECT only).
live_state() {
  printf 'ledger=%s actions=%s devices=%s aliases=%s' \
    "$(sqlite3 "$ZESTROKU_DB" "SELECT group_concat(version, ',') FROM (SELECT version FROM schema_migrations ORDER BY version)")" \
    "$(sqlite3 "$ZESTROKU_DB" "SELECT group_concat(id || ':' || decision || ':' || result, ',') FROM (SELECT id, decision, result FROM actions ORDER BY id)")" \
    "$(sqlite3 "$ZESTROKU_DB" "SELECT group_concat(id, ',') FROM (SELECT id FROM devices ORDER BY id)")" \
    "$(sqlite3 "$ZESTROKU_DB" "SELECT group_concat(alias || '>' || device_id, ',') FROM (SELECT alias, device_id FROM aliases ORDER BY alias)")"
}
# Domain readback through the installed binary (real CLI) plus read-only rows.
readback() {
  run "$1-alias-ls" "$BIN" alias ls
  [ "$RC" -eq 0 ] || fail "$1: alias ls rc=$RC: $(cat "$WORK/err")"
  RB_ALIAS=$(cat "$WORK/out")
  run "$1-log-json" "$BIN" log --json
  [ "$RC" -eq 0 ] || fail "$1: log --json rc=$RC: $(cat "$WORK/err")"
  RB_LOG=$(cat "$WORK/out")
  RB_DEV=$(sqlite3 "$ZESTROKU_DB" "SELECT id, name, ip, model, model_num, firmware, wifi_driver, access_tier, last_seen FROM devices ORDER BY id")
  RB_LEDGER=$(sqlite3 "$ZESTROKU_DB" "SELECT group_concat(version, ',') FROM (SELECT version FROM schema_migrations ORDER BY version)")
}

# S1: fresh install of the old build.
run s1-install-old "$INSTALL" --prefix "$PREFIX" --artifact "$OLD" --sha256 "$OLD_SHA" \
  --version "$OLD_VER" --sha7 "$OLD_SHA7"
[ "$RC" -eq 0 ] || fail "S1 fresh install: $(cat "$WORK/err")"
[ "$("$BIN" --version)" = "$(ver "$OLD")" ] || fail "S1 installed identity"
untouched "S1"

# S2: refusals leave the working binary byte-identical.
run s2a-wrong-digest "$INSTALL" --prefix "$PREFIX" --artifact "$NEW" --sha256 "$OLD_SHA" \
  --version "$NEW_VER" --sha7 "$NEW_SHA7"
[ "$RC" -eq 2 ] || fail "S2a wrong digest not refused (rc=$RC)"
untouched "S2a"
printf '#!/bin/sh\necho forged\n' >"$WORK/forged"; chmod +x "$WORK/forged"
run s2b-wrong-platform "$INSTALL" --prefix "$PREFIX" --artifact "$WORK/forged" \
  --sha256 "$(sha "$WORK/forged")" --version "$NEW_VER" --sha7 "$NEW_SHA7"
[ "$RC" -eq 2 ] || fail "S2b wrong platform not refused (rc=$RC)"
untouched "S2b"
run s2c-stale-provenance "$INSTALL" --prefix "$PREFIX" --artifact "$NEW" --sha256 "$NEW_SHA" \
  --version "$NEW_VER" --sha7 "$OLD_SHA7"
[ "$RC" -eq 2 ] || fail "S2c stale/provenance input not refused (rc=$RC)"
untouched "S2c"
run s2d-failed-backup env ZESTROKU_DB="$WORK/state/missing.db" "$INSTALL" --prefix "$PREFIX" \
  --artifact "$NEW" --sha256 "$NEW_SHA" --version "$NEW_VER" --sha7 "$NEW_SHA7" \
  --data-backup "$WORK/never.db"
[ "$RC" -eq 2 ] || fail "S2d failed data backup not refused (rc=$RC)"
untouched "S2d"
[ ! -e "$WORK/never.db" ] || fail "S2d: a backup file appeared for a missing store"

# S3: upgrade with a pre-upgrade backup. The OLD build creates the store; the
# seeds are the domain data that must survive a rollback.
run s3-old-creates-store "$BIN" alias ls
[ "$RC" -eq 0 ] || fail "S3 old build could not open the store: $(cat "$WORK/err")"
sqlite3 "$ZESTROKU_DB" <<'SQL'
INSERT INTO devices VALUES ('dev-a', 'Living Room', '192.0.2.10', 'Express 4K+', '3941X', '15.0.0', 'drv', 'permissive', 1700000000);
INSERT INTO devices VALUES ('dev-b', 'Bench TV', '192.0.2.11', '50 inch TV', 'C000X', '14.0.0', 'drv', 'limited', 1700000001);
INSERT INTO aliases VALUES ('living', 'dev-a');
INSERT INTO aliases VALUES ('tv', 'dev-b');
INSERT INTO actions VALUES (1, 1700000100, 'cli', 'keypress', 'Home', 'approved', 'completed', '{"app":"home"}');
INSERT INTO actions VALUES (2, 1700000200, 'cli', 'launch', '12', 'denied', 'denied', '{}');
SQL
readback "pre"
PRE_ALIAS=$RB_ALIAS; PRE_LOG=$RB_LOG; PRE_DEV=$RB_DEV; PRE_LEDGER=$RB_LEDGER
printf '%s' "$PRE_ALIAS" | grep -q living || fail "S3 seed: alias living not read back"
printf '%s' "$PRE_ALIAS" | grep -q tv || fail "S3 seed: alias tv not read back"
printf '%s' "$PRE_LOG" | grep -q completed || fail "S3 seed: completed history not read back"
printf '%s' "$PRE_LOG" | grep -q denied || fail "S3 seed: denied history not read back"
[ "$(printf '%s\n' "$PRE_DEV" | wc -l | tr -d ' ')" = 2 ] || fail "S3 seed: inventory rows"
case "$PRE_LEDGER" in 1,2,3,4,5,6,7,8) ;; *) fail "S3 seed: full ledger expected, got $PRE_LEDGER" ;; esac
PRE_STATE=$(live_state)

run s3-upgrade "$INSTALL" --prefix "$PREFIX" --artifact "$NEW" --sha256 "$NEW_SHA" \
  --version "$NEW_VER" --sha7 "$NEW_SHA7" --data-backup "$WORK/pre-upgrade.db"
[ "$RC" -eq 0 ] || fail "S3 upgrade: $(cat "$WORK/err")"
still_upgraded "S3"
[ -f "$WORK/pre-upgrade.db" ] || fail "S3 no pre-upgrade backup"
[ "$(sqlite3 "$WORK/pre-upgrade.db" "SELECT count(*) FROM devices")" = 2 ] || fail "S3 backup lacks the inventory"
# Identity is taken after the read check, so the S5 equality proves the
# restore never mutated the backup it consumed.
BACKUP_SHA=$(sha "$WORK/pre-upgrade.db")
echo "backup: $WORK/pre-upgrade.db sha256=$BACKUP_SHA"
# The candidate runs against the live store and writes (a failed upgrade).
run s3-new-opens-store "$BIN" alias ls
[ "$RC" -eq 0 ] || fail "S3 new build could not open the store: $(cat "$WORK/err")"
sqlite3 "$ZESTROKU_DB" "INSERT INTO actions VALUES (3, 1700000300, 'cli', 'launch', '13', 'approved', 'completed', '{}');"
[ "$(live_state)" != "$PRE_STATE" ] || fail "S3 post-upgrade drift fixture"

# S3b: deterministic FAILED MIGRATION of the candidate. Removing the ledger
# rows for versions 7 and 8 while their objects remain makes migration 7
# (CREATE TABLE policy_hits) impossible: the candidate cannot open the store.
sqlite3 "$ZESTROKU_DB" "DELETE FROM schema_migrations WHERE version >= 7;"
run s3b-failed-migration "$BIN" alias ls
[ "$RC" -ne 0 ] || fail "S3b the candidate must not open a store it cannot migrate"
[ "$(sqlite3 "$ZESTROKU_DB" "SELECT group_concat(version, ',') FROM (SELECT version FROM schema_migrations ORDER BY version)")" = "1,2,3,4,5,6" ] \
  || fail "S3b the failed migration must leave the ledger at 6"
BROKEN_STATE=$(live_state)

# S4: a non-database restore source is refused (by the prior-binary probe
# first); binary and data untouched.
printf 'not a database' >"$WORK/garbage.bak"
run s4-garbage-restore "$INSTALL" --prefix "$PREFIX" --rollback --restore-data "$WORK/garbage.bak"
[ "$RC" -eq 2 ] || fail "S4 garbage restore not refused (rc=$RC)"
still_upgraded "S4"
[ "$(live_state)" = "$BROKEN_STATE" ] || fail "S4 data changed by a refused restore"

# S4b: destination-version gate. A backup too new for the PRIOR binary is
# refused by the prior binary itself, before any restore or promotion.
cp "$WORK/pre-upgrade.db" "$WORK/future.bak"
sqlite3 "$WORK/future.bak" "INSERT INTO schema_migrations (version, name, applied_at) VALUES (99, 'forged-future', 0);"
run s4b-newer-backup "$INSTALL" --prefix "$PREFIX" --rollback --restore-data "$WORK/future.bak"
[ "$RC" -eq 2 ] || fail "S4b newer-schema backup not refused (rc=$RC)"
grep -q "prior binary cannot open" "$WORK/err" || fail "S4b refusal must come from the prior-binary probe: $(cat "$WORK/err")"
still_upgraded "S4b"
[ "$(live_state)" = "$BROKEN_STATE" ] || fail "S4b data changed"

# S4c: the same gate guards a binary-only rollback when the live store is
# newer than the prior binary supports.
sqlite3 "$ZESTROKU_DB" "INSERT INTO schema_migrations (version, name, applied_at) VALUES (99, 'forged-future', 0);"
run s4c-binary-only-rollback "$INSTALL" --prefix "$PREFIX" --rollback
[ "$RC" -eq 2 ] || fail "S4c binary-only rollback over newer data not refused (rc=$RC)"
grep -q "prior binary cannot open" "$WORK/err" || fail "S4c refusal must come from the prior-binary probe: $(cat "$WORK/err")"
still_upgraded "S4c"
sqlite3 "$ZESTROKU_DB" "DELETE FROM schema_migrations WHERE version = 99;"
[ "$(live_state)" = "$BROKEN_STATE" ] || fail "S4c data changed"

# S4d: a valid SQLite file with no schema ledger passes the prior-binary
# probe (it opens as a fresh store) and is refused by `db restore` ITSELF.
sqlite3 "$WORK/bare.bak" "CREATE TABLE other (x TEXT);"
run s4d-no-ledger-restore "$INSTALL" --prefix "$PREFIX" --rollback --restore-data "$WORK/bare.bak"
[ "$RC" -eq 2 ] || fail "S4d valid database without a ledger not refused (rc=$RC)"
grep -q "no schema ledger" "$WORK/err" || fail "S4d refusal must come from db restore: $(cat "$WORK/err")"
still_upgraded "S4d"
[ "$(live_state)" = "$BROKEN_STATE" ] || fail "S4d data changed"

# S4e: a restore that cannot complete (read-only state directory) leaves
# binary and data untouched, with no staging file; the retry below succeeds.
chmod 555 "$WORK/state"
run s4e-interrupted-restore "$INSTALL" --prefix "$PREFIX" --rollback --restore-data "$WORK/pre-upgrade.db"
chmod 755 "$WORK/state"
[ "$RC" -eq 2 ] || fail "S4e interrupted restore not refused (rc=$RC)"
grep -q "data restore refused" "$WORK/err" || fail "S4e refusal must come from db restore: $(cat "$WORK/err")"
[ ! -e "$WORK/state/.restore.tmp" ] || fail "S4e staging file left behind"
still_upgraded "S4e"
[ "$(live_state)" = "$BROKEN_STATE" ] || fail "S4e data changed"

# S5: failed-candidate rollback restores the pre-upgrade data, then the binary.
run s5-rollback "$INSTALL" --prefix "$PREFIX" --rollback --restore-data "$WORK/pre-upgrade.db"
[ "$RC" -eq 0 ] || fail "S5 rollback: $(cat "$WORK/err")"
[ "$(sha "$BIN")" = "$OLD_SHA" ] || fail "S5 old build not restored"
[ ! -e "$PREV" ] || fail "S5 prior slot should be consumed"
[ "$(sha "$WORK/pre-upgrade.db")" = "$BACKUP_SHA" ] || fail "S5 the backup itself changed"
[ "$(live_state)" = "$PRE_STATE" ] || fail "S5 live state differs from the pre-upgrade state"
readback "post"
[ "$RB_ALIAS" = "$PRE_ALIAS" ] || fail "S5 alias readback differs from pre-upgrade"
[ "$RB_LOG" = "$PRE_LOG" ] || fail "S5 history readback differs from pre-upgrade"
[ "$RB_DEV" = "$PRE_DEV" ] || fail "S5 inventory differs from pre-upgrade"
[ "$RB_LEDGER" = "$PRE_LEDGER" ] || fail "S5 ledger differs from pre-upgrade"

echo "evidence: $EVID ($STEP commands)"
echo "SCENARIO PASS: S1-S5 (old $(ver "$OLD") / new $(ver "$NEW"))"
