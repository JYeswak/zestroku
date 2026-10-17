#!/bin/sh
# Install (or roll back) the source-bound zestroku Mac binary at the stable
# command path (1r4.9). Refuses wrong-platform, wrong-digest, stale-input
# and provenance-mismatched candidates BEFORE touching the working binary,
# preserves the prior version, and promotes atomically.
#
#   install-macos.sh --artifact <file> --sha256 <hex> --version <semver> --sha7 <hex7> [--prefix DIR]
#   install-macos.sh --rollback [--prefix DIR]
#
# Prefix defaults to $HOME/.local; the stable path is $PREFIX/bin/zestroku
# and the preserved prior version is $PREFIX/bin/zestroku.prev. Exit 0 on
# success; exit 2 on any refusal (nothing replaced); exit 1 on tool errors.
set -eu

PREFIX="${HOME}/.local"
MODE=install
ARTIFACT=""; EXPECT_SHA=""; EXPECT_VERSION=""; EXPECT_SHA7=""

while [ $# -gt 0 ]; do
  case "$1" in
    --artifact) ARTIFACT="${2:-}"; shift 2 ;;
    --sha256) EXPECT_SHA="${2:-}"; shift 2 ;;
    --version) EXPECT_VERSION="${2:-}"; shift 2 ;;
    --sha7) EXPECT_SHA7="${2:-}"; shift 2 ;;
    --prefix) PREFIX="${2:-}"; shift 2 ;;
    --rollback) MODE=rollback; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

BIN="$PREFIX/bin/zestroku"
PREV="$PREFIX/bin/zestroku.prev"

refuse() { echo "install-macos: REFUSED: $1" >&2; exit 2; }

if [ "$MODE" = rollback ]; then
  [ -f "$PREV" ] || refuse "no preserved prior version at $PREV"
  mv -f "$PREV" "$BIN"
  echo "rolled back to: $("$BIN" --version)"
  exit 0
fi

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

# Preserve the working binary before promotion (prior-version slot).
if [ -f "$BIN" ]; then
  cp -f "$BIN" "$PREV"
  echo "preserved prior: $("$PREV" --version)"
fi
mv -f "$STAGED" "$BIN"
echo "installed: $("$BIN" --version) at $BIN"
