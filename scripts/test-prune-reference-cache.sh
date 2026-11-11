#!/usr/bin/env bash
# Checks scripts/prune-reference-cache.sh against a stand-in reference cache.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
prune=$here/prune-reference-cache.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail() {
  echo "FAIL: $*" >&2
  exit 1
}

cache=$work/ref-classes
entry() { # <name> <marker or -> <age in days>
  mkdir -p "$cache/$1/main/pkg"
  printf 'class' >"$cache/$1/main/pkg/A.class"
  if [ "$2" != - ]; then
    printf '%s' "${4:-}" >"$cache/$1/$2"
    touch -d "-$3 days" "$cache/$1/$2"
  fi
  # Directory and class files carry an old time either way: only the marker counts as a read.
  touch -d '-90 days' "$cache/$1/main/pkg/A.class" "$cache/$1"
}
entry fresh-ok OK 0
entry recent-ok OK 6
entry stale-ok OK 8
entry fresh-failed FAILED 1 'a.kt:1:1: error: x'
entry stale-failed FAILED 30 'a.kt:1:1: error: x'
entry 0123.stage42-0 - 0

out=$("$prune" "$cache" 7)
[ "$out" = "reference cache: kept 3 entries, dropped 3 unread for 7+ days or unpublished" ] \
  || fail "summary: $out"
for kept in fresh-ok recent-ok fresh-failed; do
  [ -f "$cache/$kept/main/pkg/A.class" ] || fail "$kept was dropped"
done
for dropped in stale-ok stale-failed 0123.stage42-0; do
  [ ! -e "$cache/$dropped" ] || fail "$dropped was kept"
done

out=$("$prune" "$work/missing" 7)
[ -z "$out" ] || fail "a missing cache printed: $out"

for bad in 0 -1 x ''; do
  if "$prune" "$cache" "$bad" 2>/dev/null; then fail "accepted max age '$bad'"; fi
done
[ -d "$cache/fresh-ok" ] || fail "a rejected call pruned"

echo "prune-reference-cache: all checks pass"
