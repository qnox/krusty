#!/usr/bin/env bash
# The platform/version outcome manifests only shrink: a change may remove entries, never add one, so a
# regression cannot be recorded as expected and a fix cannot pay for one. That covers the box
# expected failures and not-applicable lists, the CLI corpus expected failures, and uncovered KLIB
# implementations. A release records a manifest only where it differs from its predecessor's, and a
# release with none reads the newest one below it, so what is compared is the manifest each release
# READS: deleting a release's own file is an addition when the inherited one lists more. A release
# the base did not support yet (a newly supported Kotlin version, or a new target) is exempt. Only
# the head's own changes count: it is compared with its merge base, so a branch that is behind a
# base which has since dropped entries is not charged with them. The CLI not-applicable lists are
# not checked here: they record what the reference kotlinc cannot run in this environment, and the
# required reference run proves every entry instead.
#
#   scripts/check-outcome-lists.sh <base-rev> <head-rev>
set -euo pipefail

if [ "$#" -ne 2 ]; then
  sed -n '2,14p' "$0" >&2
  exit 2
fi
head=$2
base=$(git merge-base "$1" "$head")

entries() {
  git show "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' | grep -v -e '^$' -e '^#' | sort -u || true
}

# The releases a revision supports: its kotlin-versions manifest, or, without one, the releases its
# lane records.
releases() {
  local rev=$1 lane=$2
  if git cat-file -e "$rev:kotlin-versions" 2>/dev/null; then
    git show "$rev:kotlin-versions" | sed 's/#.*//' | awk 'NF { print $1 }'
  else
    recorded "$rev" "$lane"
  fi | sort -uV
}

recorded() {
  git ls-tree --name-only "$1" -- "$2/" 2>/dev/null |
    sed -n 's|.*/\([0-9]*\.[0-9]*\.[0-9]*\)\.txt$|\1|p' | sort -uV
}

# The manifest `release` reads at a revision: its own, else the newest recorded supported release
# below it. Prints nothing when none applies.
reads() {
  local rev=$1 lane=$2 release=$3 candidates
  candidates=$(recorded "$rev" "$lane" | grep -Fx -f <(releases "$rev" "$lane") || true)
  [ -n "$candidates" ] || return 0
  { echo "$candidates"; echo "$release"; } | sort -V | sed "/^${release//./\\.}\$/q" |
    grep -Fx -f <(echo "$candidates") | tail -n 1 || true
}

added_total=0
while read -r lane; do
  base_releases=$(releases "$base" "$lane")
  for release in $( { echo "$base_releases"; releases "$head" "$lane"; recorded "$head" "$lane"; } | sed '/^$/d' | sort -uV); do
    grep -qx -- "$release" <<<"$base_releases" || continue
    from=$(reads "$base" "$lane" "$release")
    [ -n "$from" ] || continue
    to=$(reads "$head" "$lane" "$release")
    if [ -n "$to" ]; then
      added=$(comm -13 <(entries "$base:$lane/$from.txt") <(entries "$head:$lane/$to.txt"))
    else
      added=""
    fi
    [ -n "$added" ] || continue
    count=$(wc -l <<<"$added" | tr -d '[:space:]')
    added_total=$((added_total + count))
    manifest="$lane/$release.txt"
    [ "$to" = "$release" ] || manifest="$manifest (reads $lane/$to.txt)"
    echo "outcome-lists: $manifest gains $count entr$([ "$count" -eq 1 ] && echo y || echo ies):" >&2
    sed 's/^/    /' <<<"$added" >&2
  done
done < <(
  { git ls-tree -r --name-only "$base"; git ls-tree -r --name-only "$head"; } |
    grep -E '^tests/(box_expected_failures|box_expected_not_applicable|cli_expected_failures|klib_uncovered_implementations)/[^/]+/[^/]+\.txt$' |
    sed 's|/[^/]*$||' | sort -u
)

if ((added_total)); then
  echo "outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them" >&2
  exit 1
fi
