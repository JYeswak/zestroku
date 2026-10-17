#!/bin/sh
# Consumer scenario for scripts/install-macos.sh (1r4.9): two real source-bound
# Mach-O builds, a real Store and the real `zestroku db` verbs. Fresh-shell
# usage, on a Mac:
#
#   scripts/test-install-macos.sh <old-binary> <new-binary>
#
# The two binaries must be builds of two REAL saved revisions, each carrying
# its own `zestroku <semver> <sha7>` identity (the sha7 of the commit it was
# built from), and neither may be -dirty. Never relabel one source tree with
# another revision's identity: that is false provenance. The old revision
# may predate the `db` verb; rollback therefore restores data with the NEW
# binary first. Scratch goes under $TMPDIR (agents: set it to the repo's
# var/agent-tmp dir first); it is left for the scratch reaper.
#
# Scenarios: S1 fresh install; S2 refusals (wrong digest, wrong platform,
# stale/provenance input, failed data backup) each leaving the working binary
# byte-identical; S3 upgrade with a pre-upgrade backup; S4 an unsafe restore
# (non-database) refused with binary AND data untouched; S5 failed-candidate
# rollback restoring the pre-upgrade data. stock sqlite3 only seeds and reads
# a marker row; it is never the backup or restore client.
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
# Run a command capturing its exit code without tripping `set -e`.
run() { set +e; "$@" >"$WORK/out" 2>"$WORK/err"; RC=$?; set -e; }

[ -x "$OLD" ] && [ -x "$NEW" ] || fail "both binaries must be executable files"
[ "$(ver "$OLD")" != "$(ver "$NEW")" ] || fail "old and new must carry distinct identities"
case "$(ver "$OLD") $(ver "$NEW")" in
  *-dirty*) fail "a -dirty build is never acceptable evidence" ;;
esac
command -v sqlite3 >/dev/null || fail "sqlite3 is required to seed and read the marker row"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/zestroku-install.XXXXXX")
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

install_old() {
  run "$INSTALL" --prefix "$PREFIX" --artifact "$OLD" --sha256 "$OLD_SHA" \
    --version "$OLD_VER" --sha7 "$OLD_SHA7"
}
untouched() { # the working binary is still the old build and nothing was staged
  [ "$(sha "$BIN")" = "$OLD_SHA" ] || fail "$1: working binary changed"
  [ ! -e "$PREFIX/bin/.zestroku.new" ] || fail "$1: staged candidate left behind"
  [ ! -e "$PREV" ] || fail "$1: a prior-version slot appeared"
}
marker() { sqlite3 "$ZESTROKU_DB" "SELECT group_concat(v, ',') FROM (SELECT v FROM scenario_marker ORDER BY rowid)"; }

# S1: fresh install of the old build.
install_old
[ "$RC" -eq 0 ] || fail "S1 fresh install: $(cat "$WORK/err")"
[ "$("$BIN" --version)" = "$(ver "$OLD")" ] || fail "S1 installed identity"
untouched "S1"

# S2: refusals leave the working binary byte-identical.
run "$INSTALL" --prefix "$PREFIX" --artifact "$NEW" --sha256 "$OLD_SHA" \
  --version "$NEW_VER" --sha7 "$NEW_SHA7"
[ "$RC" -eq 2 ] || fail "S2a wrong digest not refused (rc=$RC)"
untouched "S2a"
printf '#!/bin/sh\necho forged\n' >"$WORK/forged"; chmod +x "$WORK/forged"
run "$INSTALL" --prefix "$PREFIX" --artifact "$WORK/forged" --sha256 "$(sha "$WORK/forged")" \
  --version "$NEW_VER" --sha7 "$NEW_SHA7"
[ "$RC" -eq 2 ] || fail "S2b wrong platform not refused (rc=$RC)"
untouched "S2b"
run "$INSTALL" --prefix "$PREFIX" --artifact "$NEW" --sha256 "$NEW_SHA" \
  --version "$NEW_VER" --sha7 "$OLD_SHA7"
[ "$RC" -eq 2 ] || fail "S2c stale/provenance input not refused (rc=$RC)"
untouched "S2c"
run env ZESTROKU_DB="$WORK/state/missing.db" "$INSTALL" --prefix "$PREFIX" --artifact "$NEW" \
  --sha256 "$NEW_SHA" --version "$NEW_VER" --sha7 "$NEW_SHA7" --data-backup "$WORK/never.db"
[ "$RC" -eq 2 ] || fail "S2d failed data backup not refused (rc=$RC)"
untouched "S2d"
[ ! -e "$WORK/never.db" ] || fail "S2d: a backup file appeared for a missing store"

# S3: upgrade with a pre-upgrade backup. The old build creates the store; the
# fixture row is the data that must survive a rollback.
run "$BIN" alias ls
[ "$RC" -eq 0 ] || fail "S3 old build could not open the store: $(cat "$WORK/err")"
sqlite3 "$ZESTROKU_DB" "CREATE TABLE scenario_marker(v TEXT); INSERT INTO scenario_marker VALUES ('pre-upgrade');"
run "$INSTALL" --prefix "$PREFIX" --artifact "$NEW" --sha256 "$NEW_SHA" \
  --version "$NEW_VER" --sha7 "$NEW_SHA7" --data-backup "$WORK/pre-upgrade.db"
[ "$RC" -eq 0 ] || fail "S3 upgrade: $(cat "$WORK/err")"
[ "$(sha "$BIN")" = "$NEW_SHA" ] || fail "S3 new build not promoted"
[ "$(sha "$PREV")" = "$OLD_SHA" ] || fail "S3 prior version not preserved"
[ -f "$WORK/pre-upgrade.db" ] || fail "S3 no pre-upgrade backup"
[ "$(sqlite3 "$WORK/pre-upgrade.db" "SELECT v FROM scenario_marker")" = "pre-upgrade" ] \
  || fail "S3 backup lacks the marker row"
# Identity is taken after the read check, so the S5 equality proves the
# restore never mutated the backup it consumed.
BACKUP_SHA=$(sha "$WORK/pre-upgrade.db")
echo "backup: $WORK/pre-upgrade.db sha256=$BACKUP_SHA"
# The candidate runs against the live store and writes (a failed upgrade).
run "$BIN" alias ls
[ "$RC" -eq 0 ] || fail "S3 new build could not open the store: $(cat "$WORK/err")"
sqlite3 "$ZESTROKU_DB" "INSERT INTO scenario_marker VALUES ('post-upgrade');"
[ "$(marker)" = "pre-upgrade,post-upgrade" ] || fail "S3 drift fixture"

# S4: an unsafe restore (not a database) is refused; binary and data untouched.
printf 'not a database' >"$WORK/garbage.bak"
run "$INSTALL" --prefix "$PREFIX" --rollback --restore-data "$WORK/garbage.bak"
[ "$RC" -eq 2 ] || fail "S4 garbage restore not refused (rc=$RC)"
[ "$(sha "$BIN")" = "$NEW_SHA" ] || fail "S4 binary changed by a refused restore"
[ "$(sha "$PREV")" = "$OLD_SHA" ] || fail "S4 prior slot changed by a refused restore"
[ "$(marker)" = "pre-upgrade,post-upgrade" ] || fail "S4 data changed by a refused restore"

# S5: failed-candidate rollback restores the pre-upgrade data, then the binary.
run "$INSTALL" --prefix "$PREFIX" --rollback --restore-data "$WORK/pre-upgrade.db"
[ "$RC" -eq 0 ] || fail "S5 rollback: $(cat "$WORK/err")"
[ "$(sha "$BIN")" = "$OLD_SHA" ] || fail "S5 old build not restored"
[ ! -e "$PREV" ] || fail "S5 prior slot should be consumed"
[ "$(marker)" = "pre-upgrade" ] || fail "S5 data not rolled back: $(marker)"
[ "$(sha "$WORK/pre-upgrade.db")" = "$BACKUP_SHA" ] || fail "S5 the backup itself changed"
run "$BIN" alias ls
[ "$RC" -eq 0 ] || fail "S5 old build cannot open the restored store: $(cat "$WORK/err")"

echo "SCENARIO PASS: S1-S5 (old $(ver "$OLD") / new $(ver "$NEW"))"
