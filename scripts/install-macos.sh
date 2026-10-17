#!/bin/sh
# Install (or roll back) the source-bound zestroku Mac binary at the stable
# command path, together with a pre-upgrade data backup (1r4.9). Refuses
# wrong-platform, wrong-digest, stale-input and provenance-mismatched
# candidates BEFORE touching the working binary, preserves the prior
# version, and promotes atomically.
#
#   install-macos.sh --artifact <file> --sha256 <hex> --version <semver> \
#       --sha7 <hex7> [--data-backup <file>] [--prefix DIR]
#   install-macos.sh --rollback [--restore-data <backup-file>] [--prefix DIR]
#
# Prefix defaults to $HOME/.local; the stable path is $PREFIX/bin/zestroku
# and the preserved prior version is $PREFIX/bin/zestroku.prev.
#
# Data (ZESTROKU_DB selects the store, as for the binary itself):
#   --data-backup FILE   before promotion, the CANDIDATE takes a snapshot of
#                        the store at the schema it is at (`zestroku db
#                        backup`, which never migrates). A failed backup
#                        refuses the install; nothing is replaced.
#   --restore-data FILE  on rollback, the CURRENT binary first restores the
#                        store from FILE (`zestroku db restore`, validated:
#                        SQLite header, schema ledger it supports, no live
#                        writer); only then is the prior binary promoted. A
#                        refused restore leaves binary and data untouched.
#
# Services: this script never stops, starts or signals the resident daemon.
# Stop it through the approved operator route before --restore-data (the
# restore refuses while a writer holds the store) and restart it after.
#
# Exit 0 on success; exit 2 on any refusal (nothing replaced); exit 1 on
# tool errors.
set -eu

PREFIX="${HOME}/.local"
MODE=install
ARTIFACT=""; EXPECT_SHA=""; EXPECT_VERSION=""; EXPECT_SHA7=""
DATA_BACKUP=""; RESTORE_DATA=""

while [ $# -gt 0 ]; do
  case "$1" in
    --artifact) ARTIFACT="${2:-}"; shift 2 ;;
    --sha256) EXPECT_SHA="${2:-}"; shift 2 ;;
    --version) EXPECT_VERSION="${2:-}"; shift 2 ;;
    --sha7) EXPECT_SHA7="${2:-}"; shift 2 ;;
    --prefix) PREFIX="${2:-}"; shift 2 ;;
    --data-backup) DATA_BACKUP="${2:-}"; shift 2 ;;
    --restore-data) RESTORE_DATA="${2:-}"; shift 2 ;;
    --rollback) MODE=rollback; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

BIN="$PREFIX/bin/zestroku"
PREV="$PREFIX/bin/zestroku.prev"

refuse() { echo "install-macos: REFUSED: $1" >&2; exit 2; }

if [ "$MODE" = rollback ]; then
  [ -z "$DATA_BACKUP" ] || refuse "--data-backup applies to install, not rollback"
  [ -f "$PREV" ] || refuse "no preserved prior version at $PREV"
  if [ -n "$RESTORE_DATA" ]; then
    [ -f "$RESTORE_DATA" ] || refuse "restore source is not a file: $RESTORE_DATA"
    [ -x "$BIN" ] || refuse "no current binary at $BIN to validate the restore"
    # The current (newer) binary restores first: it knows every schema the
    # prior one could have written, and a refusal here leaves everything as
    # it was. The prior binary may predate the `db` verb entirely.
    "$BIN" db restore "$RESTORE_DATA" || refuse "data restore refused; binary and data untouched"
    echo "restored data from: $RESTORE_DATA sha256=$(shasum -a 256 "$RESTORE_DATA" | awk '{print $1}')"
  fi
  mv -f "$PREV" "$BIN"
  echo "rolled back to: $("$BIN" --version)"
  exit 0
fi

[ -z "$RESTORE_DATA" ] || refuse "--restore-data applies to rollback, not install"
[ -n "$ARTIFACT" ] || refuse "missing --artifact"
[ -n "$EXPECT_SHA" ] || refuse "missing --sha256"
[ -n "$EXPECT_VERSION" ] || refuse "missing --version"
[ -n "$EXPECT_SHA7" ] || refuse "missing --sha7"
[ -f "$ARTIFACT" ] || refuse "artifact is not a file: $ARTIFACT"

# Wrong-digest refusal comes before any execution of the candidate.
ACTUAL_SHA="$(shasum -a 256 "$ARTIFACT" | awk '{print $1}')"
[ "$ACTUAL_SHA" = "$EXPECT_SHA" ] || refuse "digest mismatch for $ARTIFACT"

# Wrong-platform refusal: source-bound Mac product binary only.
KIND="$(file -b "$ARTIFACT")"
case "$KIND" in
  *Mach-O*arm64*) ;;
  *) refuse "not an arm64 Mach-O binary: $KIND" ;;
esac

# Provenance refusal: the candidate must name its exact published source.
STAGED="$PREFIX/bin/.zestroku.new"
mkdir -p "$PREFIX/bin"
cp -f "$ARTIFACT" "$STAGED"
chmod +x "$STAGED"
ACTUAL_VERSION="$("$STAGED" --version 2>/dev/null || true)"
EXPECTED="zestroku $EXPECT_VERSION $EXPECT_SHA7"
[ "$ACTUAL_VERSION" = "$EXPECTED" ] || {
  rm -f "$STAGED"
  refuse "provenance mismatch: got '$ACTUAL_VERSION', want '$EXPECTED'"
}

# Pre-upgrade data backup, taken by the verified candidate BEFORE anything is
# replaced or migrated: `db backup` snapshots at the schema the data is at.
if [ -n "$DATA_BACKUP" ]; then
  "$STAGED" db backup "$DATA_BACKUP" || {
    rm -f "$STAGED"
    refuse "pre-upgrade data backup failed; nothing replaced"
  }
  echo "data backup: $DATA_BACKUP sha256=$(shasum -a 256 "$DATA_BACKUP" | awk '{print $1}')"
fi

# Preserve the working binary before promotion (prior-version slot).
if [ -f "$BIN" ]; then
  cp -f "$BIN" "$PREV"
  echo "preserved prior: $("$PREV" --version)"
fi
mv -f "$STAGED" "$BIN"
echo "installed: $("$BIN" --version) at $BIN"
