#!/usr/bin/env bash
# Identifier scrub gate (plan ZP:58-59): device identifiers never land in git.
# var/ and target/ are untracked working areas, so only tracked content is
# scanned — anything git add-ed (even under var/) IS scanned. Scrubbed
# placeholders (FIXTURESERIAL, <serial>, "scrubbed") pass; real shapes fail.
set -euo pipefail

fail=0
patterns=(
  '([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}'                          # MAC
  '\bUDN:[a-zA-Z0-9-]{16,}'                                     # UPnP UDN
  '"(ad-id|adId|device-id|serial|serialNumber)"[[:space:]]*:'   # identifier JSON keys → value checked below
)

files=$( (git ls-files; git diff --cached --name-only) | sort -u )
if [ -z "$files" ]; then exit 0; fi

while IFS= read -r file; do
  [ -f "$file" ] || continue
  for pat in "${patterns[@]}"; do
    hits=$(grep -En "$pat" "$file" || true)
    [ -z "$hits" ] && continue
    # a line passes only if every hit is an explicit scrubbed placeholder
    bad=$(printf '%s\n' "$hits" | grep -vcE 'FIXTURE|placeholder|<serial>|scrubbed' || true)
    if [ "$bad" -gt 0 ]; then
      echo "identifier-shaped value in $file (pattern $pat):"
      printf '%s\n' "$hits" | grep -vE 'FIXTURE|placeholder|<serial>|scrubbed' | head -3
      fail=1
    fi
  done
done <<< "$files"

exit $fail
