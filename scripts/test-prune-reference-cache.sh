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
entry fresh-rejected REJECTED 1 'main
a.kt:1:1: error: x
'
entry stale-rejected REJECTED 30 'main
a.kt:1:1: error: x
'
entry 0123.stage42-0 - 0

out=$("$prune" "$cache" 7)
[ "$out" = "reference cache: kept 4 entries, dropped 4 unread for 7+ days or unpublished" ] \
  || fail "summary: $out"
for kept in fresh-ok recent-ok fresh-failed fresh-rejected; do
  [ -f "$cache/$kept/main/pkg/A.class" ] || fail "$kept was dropped"
done
for dropped in stale-ok stale-failed stale-rejected 0123.stage42-0; do
  [ ! -e "$cache/$dropped" ] || fail "$dropped was kept"
done

# A read-since file keeps only the entries read after it, within the age limit.
entry seeded-ok OK 2
entry read-ok OK 0
touch -d '-1 days' "$work/restored"
touch "$cache/read-ok/OK"
out=$("$prune" "$cache" 7 "$work/restored")
[ "$out" = "reference cache: kept 2 entries, dropped 4 unread for 7+ days, unread since $work/restored, or unpublished" ] \
  || fail "read-since summary: $out"
for kept in fresh-ok read-ok; do
  [ -f "$cache/$kept/main/pkg/A.class" ] || fail "$kept was dropped with a read-since file"
done
for dropped in recent-ok fresh-failed fresh-rejected seeded-ok; do
  [ ! -e "$cache/$dropped" ] || fail "$dropped was kept with a read-since file"
done
if "$prune" "$cache" 7 "$work/absent" 2>/dev/null; then fail "accepted a missing read-since file"; fi

out=$("$prune" "$work/missing" 7)
[ -z "$out" ] || fail "a missing cache printed: $out"

for bad in 0 -1 x ''; do
  if "$prune" "$cache" "$bad" 2>/dev/null; then fail "accepted max age '$bad'"; fi
done
[ -d "$cache/fresh-ok" ] || fail "a rejected call pruned"

echo "prune-reference-cache: all checks pass"
