#!/usr/bin/env bash
# The platform/version outcome manifests only shrink: a change may remove entries, never add one, so a
# regression cannot be recorded as expected and a fix cannot pay for one. That covers the box
# expected failures and not-applicable lists, the CLI corpus expected failures, and uncovered KLIB
# externals. A manifest the base does not have yet (a newly supported Kotlin version or target) is
# exempt. Only the head's own changes count: it is compared with its merge base, so a branch that
# is behind a base which has since dropped entries is not charged with them. The CLI not-applicable
# lists are not checked here: they record what the reference kotlinc cannot run in this environment,
# and the required reference run proves every entry instead.
#
#   scripts/check-outcome-lists.sh <base-rev> <head-rev>
set -euo pipefail

if [ "$#" -ne 2 ]; then
  sed -n '2,12p' "$0" >&2
  exit 2
fi
head=$2
base=$(git merge-base "$1" "$head")

entries() {
  git show "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' | grep -v -e '^$' -e '^#' | sort -u || true
}

added_total=0
while read -r manifest; do
  git cat-file -e "$base:$manifest" 2>/dev/null || continue
  added=$(comm -13 <(entries "$base:$manifest") <(entries "$head:$manifest"))
  [ -n "$added" ] || continue
  count=$(wc -l <<<"$added" | tr -d '[:space:]')
  added_total=$((added_total + count))
  echo "outcome-lists: $manifest gains $count entr$([ "$count" -eq 1 ] && echo y || echo ies):" >&2
  sed 's/^/    /' <<<"$added" >&2
done < <(
  git ls-tree -r --name-only "$head" -- \
    tests/box_expected_failures \
    tests/box_expected_not_applicable \
    tests/cli_expected_failures \
    tests/klib_uncovered_externals
)

if ((added_total)); then
  echo "outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them" >&2
  exit 1
fi
