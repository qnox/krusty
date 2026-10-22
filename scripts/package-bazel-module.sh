#!/usr/bin/env bash
# Package the krusty Bazel module of one release.
#
#   usage: package-bazel-module.sh VERSION DIST_DIRECTORY
#
# DIST_DIRECTORY holds the release's compiler archives (krusty-VERSION-<triple>.tar.gz/.zip). The
# module's MODULE.bazel gets VERSION and a `krusty.release(...)` tag naming each archive's SHA-256,
# so the released module downloads its own compiler. Writes krusty-bazel-VERSION.tar.gz and its
# .sha256 into DIST_DIRECTORY and prints the archive's path. Two runs over the same inputs write the
# same bytes.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 VERSION DIST_DIRECTORY" >&2
  exit 2
fi

version=$1
dist=$2
if [[ ! $version =~ ^[0-9A-Za-z][0-9A-Za-z._+-]*$ ]]; then
  echo "invalid version: $version" >&2
  exit 2
fi

repo=$(cd "$(dirname "$0")/.." && pwd)
module=$repo/tools/krusty-bazel

# One archive for every host extensions.bzl registers a toolchain for, and none it does not know.
hosts=$(sed -nE 's/^    "([^"]+)": \["@platforms.*/\1/p' "$module/extensions.bzl")
if [[ -z $hosts ]]; then
  echo "no HOSTS entries found in $module/extensions.bzl" >&2
  exit 1
fi
release="krusty.release(
    version = \"$version\",
    sha256 = {"
for triple in $hosts; do
  archive=$dist/krusty-$version-$triple.$([[ $triple == *windows* ]] && echo zip || echo tar.gz)
  if [[ ! -f $archive ]]; then
    echo "missing compiler archive $archive" >&2
    exit 1
  fi
  release+="
        \"$triple\": \"$(sha256sum "$archive" | cut -d' ' -f1)\","
done
release+="
    },
)"
shopt -s nullglob
for archive in "$dist"/krusty-"$version"-*.tar.gz "$dist"/krusty-"$version"-*.zip; do
  triple=$(basename "$archive")
  triple=${triple#krusty-"$version"-}
  triple=${triple%.*}
  triple=${triple%.tar}
  if ! grep -qxF "$triple" <<<"$hosts"; then
    echo "$archive is for a host extensions.bzl does not know" >&2
    exit 1
  fi
done

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$module"/{BUILD.bazel,defs.bzl,extensions.bzl,toolchain.bzl,README.md} "$repo/LICENSE" "$stage/"

# Exactly one module version and one release marker, or the stamp would leave the source's in place.
source_module=$module/MODULE.bazel
for pattern in '^    version = "[^"]*",$' '^# @krusty-release@$'; do
  if [[ $(grep -c "$pattern" "$source_module") -ne 1 ]]; then
    echo "$source_module must have exactly one line matching $pattern" >&2
    exit 1
  fi
done
VERSION=$version RELEASE=$release awk '
  /^    version = "[^"]*",$/ { print "    version = \"" ENVIRON["VERSION"] "\","; next }
  /^# @krusty-release@$/ { print ENVIRON["RELEASE"]; next }
  { print }
' "$source_module" > "$stage/MODULE.bazel"

# Sorted, owner-free and timestamp-free, so the archive's checksum depends only on its contents.
out=$dist/krusty-bazel-$version.tar.gz
(cd "$stage" && find . -type f | LC_ALL=C sort | sed 's#^\./##' |
  tar --owner=0 --group=0 --numeric-owner --mtime=@0 --mode=u+rw,go+r,go-w -cf - -T - | gzip -n -9) > "$out"
(cd "$dist" && sha256sum "krusty-bazel-$version.tar.gz" > "krusty-bazel-$version.tar.gz.sha256")
echo "$out"
