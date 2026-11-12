#!/usr/bin/env bash
# Git merge driver for the outcome manifests (.gitattributes `merge=sorted-set`). Each manifest is a
# leading comment header followed by unique entries in byte order, so it is a set: two branches that
# remove or add neighbouring entries changed different elements, and a line-based merge reports a
# conflict where there is none. The merge keeps an entry exactly when both sides still have it or
# one side added it; an entry either side removed stays removed, so a merge never brings back an
# entry a fix dropped. The entries are written back in byte order under the merged header.
#
# A header both sides changed differently, or a file that is not a header plus sorted unique entries
# (a comment or blank line between entries), falls back to git's line merge and its conflict markers.
#
#   scripts/outcome-set-merge.sh <base> <ours> <theirs>
# Git passes %O %A %B; the result replaces <ours>. Install with `just install-merge-drivers`.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  sed -n '2,12p' "$0" >&2
  exit 2
fi
base=$1 ours=$2 theirs=$3
export LC_ALL=C

line_merge() {
  git merge-file -L ours -L base -L theirs "$ours" "$base" "$theirs"
}

# The leading run of comment and blank lines.
header() {
  awk '!/^#/ && !/^[[:space:]]*$/ { exit } { print }' "$1"
}

# The entries after the header, or failure when the rest is not sorted unique entries.
entries() {
  awk 'started || (!/^#/ && !/^[[:space:]]*$/) { started = 1; print }' "$1" >"$2"
  if grep -q -e '^#' -e '^[[:space:]]*$' "$2"; then
    return 1
  fi
  sort -u -c "$2" 2>/dev/null
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for side in base ours theirs; do
  if ! entries "${!side}" "$work/$side"; then
    line_merge
    exit $?
  fi
  header "${!side}" >"$work/$side.header"
done

if cmp -s "$work/ours.header" "$work/theirs.header" || cmp -s "$work/base.header" "$work/theirs.header"; then
  merged_header=$work/ours.header
elif cmp -s "$work/base.header" "$work/ours.header"; then
  merged_header=$work/theirs.header
else
  line_merge
  exit $?
fi

# Kept by both sides, or added by one: (ours ∩ theirs) ∪ (ours − base) ∪ (theirs − base).
{
  comm -12 "$work/ours" "$work/theirs"
  comm -23 "$work/ours" "$work/base"
  comm -23 "$work/theirs" "$work/base"
} | sort -u >"$work/entries"

cat "$merged_header" "$work/entries" >"$work/merged"
cat "$work/merged" >"$ours"
