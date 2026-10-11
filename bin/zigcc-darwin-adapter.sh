#!/bin/sh
# cc-rs / rustc -> zig cc adapter for the aarch64-apple-darwin cross build.
#
# WHY: `ring` (rustls dependency) compiles C and assembly through cc-rs, and
# the Contabo workers have only a Linux `cc`. The fleet shim
# `/usr/local/bin/zigcc-aarch64-darwin` already supplies `-target aarch64-macos`
# and the SDK flags, but cc-rs appends arguments zig's clang driver rejects.
# Same adapter as franken-harvest/bin/zigcc-darwin-adapter.sh, copied so a
# clean zestroku checkout builds with no box mutation.
#
# RULE for dropping an argument: only if it is (a) redundant with what the shim
# supplies or (b) unsupported by zig with no equivalent. Never drop an argument
# that carries information the compile needs; if in doubt, let it through.

set -eu

SHIM="${ZESTROKU_ZIGCC_SHIM:-/usr/local/bin/zigcc-aarch64-darwin}"
DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
case "$DEPLOYMENT_TARGET" in
    ''|*[!0-9.]*)
        printf '[ZIGCC_DEPLOYMENT_TARGET_INVALID] %s\n' "$DEPLOYMENT_TARGET" >&2
        exit 2
        ;;
esac

args=""
skip_next=0
for arg in "$@"; do
    if [ "$skip_next" -eq 1 ]; then
        skip_next=0
        continue
    fi
    case "$arg" in
        # Redundant: cc-rs emits Apple's triple spelling; the shim passes -target.
        --target=*-apple-*) continue ;;
        # Redundant: cc-rs passes -arch/-mmacosx-version-min; the shim's -target
        # below already fixes both.
        -arch) skip_next=1; continue ;;
        -mmacosx-version-min=*) continue ;;
        # zig's linker driver has no equivalent; rustc passes it as a PAIR
        # ("-Wl,-exported_symbols_list" "-Wl,/path"). Safe for an executable:
        # the list only restricts exports.
        -Wl,-exported_symbols_list) skip_next=1; continue ;;
    esac
    args="$args '$(printf '%s' "$arg" | sed "s/'/'\\\\''/g")'"
done

eval "exec \"\$SHIM\" -target 'aarch64-macos.$DEPLOYMENT_TARGET' $args"
