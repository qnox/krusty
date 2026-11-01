#!/usr/bin/env bash
# Checks one corpus project out at its pinned commit, applies its patch, and rewrites its Kotlin
# version pin to the newest release in `kotlin-versions`. `run.sh` and `bench.sh` share it.
#
#   checkout.sh <name> <checkout dir>
#
# Fails fast: an unknown project, a failed fetch, a checkout that is not the pinned commit, a patch
# that does not apply, or a version pin that does not match exactly one assignment stops it.
set -euo pipefail

name=$1
checkout=$2
here=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$here/../.." && pwd)
projects=${CORPUS_PROJECTS:-$here/projects.tsv}

row=$(awk -F'\t' -v name="$name" '$1 == name' "$projects")
if [ -z "$row" ]; then
  echo "unknown corpus project: $name" >&2
  exit 2
fi
IFS=$'\t' read -r _ url commit _ _ pin _ <<<"$row"

kotlin=$(sed 's/#.*//' "$repo_root/kotlin-versions" | awk 'NF { print $1 }' | sort -V | tail -n 1)
if [ -z "$kotlin" ]; then
  echo "kotlin-versions lists no release" >&2
  exit 2
fi

rm -rf "$checkout"
git init -q "$checkout"
git -C "$checkout" remote add origin "$url"
git -C "$checkout" fetch -q --depth 1 origin "$commit"
git -C "$checkout" checkout -q FETCH_HEAD
head=$(git -C "$checkout" rev-parse HEAD)
if [ "$head" != "$commit" ]; then
  echo "$name: checked out $head, not the pinned $commit" >&2
  exit 1
fi
if [ -f "$here/patches/$name.patch" ]; then
  git -C "$checkout" apply "$here/patches/$name.patch"
fi
if [ "$pin" != - ]; then
  python3 "$here/pin.py" "$checkout/${pin%%:*}" "${pin#*:}" "$kotlin"
fi
