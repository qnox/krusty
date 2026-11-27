#!/usr/bin/env bash
# Exercise scripts/sort-kotlin-versions.sh: a release ranks above its own pre-releases and below the
# next release, numeric parts compare as numbers, and every name comes back exactly as written.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
failures=0

expect() { # <name> <expected, one per line> <input, one per line>
  local actual
  actual=$(printf '%s\n' "$3" | "$repo/scripts/sort-kotlin-versions.sh")
  if [ "$actual" != "$2" ]; then
    echo "FAIL $1" >&2
    echo "  expected: $(tr '\n' ' ' <<<"$2")" >&2
    echo "  actual:   $(tr '\n' ' ' <<<"$actual")" >&2
    failures=$((failures + 1))
  fi
}

expect "a release follows its release candidate" $'2.5.0-RC\n2.5.0' $'2.5.0\n2.5.0-RC'
expect "pre-releases order before the release" $'2.5.0-Beta1\n2.5.0-RC\n2.5.0-RC2\n2.5.0' $'2.5.0\n2.5.0-RC2\n2.5.0-Beta1\n2.5.0-RC'
expect "a pre-release of the next release follows the previous release" $'2.4.21\n2.5.0-RC' $'2.5.0-RC\n2.4.21'
expect "numeric parts compare as numbers" $'2.4.0\n2.4.10\n2.4.20\n2.4.21' $'2.4.21\n2.4.10\n2.4.0\n2.4.20'
expect "the manifest in use" $'2.4.0\n2.4.10\n2.4.20\n2.4.21\n2.5.0-RC2' $'2.5.0-RC2\n2.4.0\n2.4.21\n2.4.10\n2.4.20'

[ "$failures" -eq 0 ] || exit 1
echo "sort-kotlin-versions: ok"
