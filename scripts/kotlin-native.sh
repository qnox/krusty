#!/usr/bin/env bash
# Provision the Kotlin/Native distribution for one Kotlin version under a cache directory and print
# its root.
#
# A cached root counts only when it holds the common stdlib KLIB's payload: its manifest and at
# least one non-empty `.knm` metadata fragment under `default/linkdata`. CI restores `target/`
# through Swatinem/rust-cache, which deletes every file outside Cargo's profile directories but
# keeps the directories, so a bare `klib/common/stdlib` tree proves nothing. An incomplete root is
# discarded and downloaded again.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <kotlin-version> <cache-dir>" >&2
  exit 2
fi
ver="$1"
cache="$2"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) host=linux-x86_64 ;;
  Linux-aarch64|Linux-arm64) host=linux-aarch64 ;;
  Darwin-x86_64) host=macos-x86_64 ;;
  Darwin-arm64|Darwin-aarch64) host=macos-aarch64 ;;
  *) echo "unsupported Kotlin/Native host: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
name="kotlin-native-prebuilt-${host}-${ver}"
parent="$cache/$ver"
root="$parent/$name"
# Releases are fetched from GitHub; KRUSTY_KOTLIN_NATIVE_RELEASES names another base (a mirror).
releases="${KRUSTY_KOTLIN_NATIVE_RELEASES:-https://github.com/JetBrains/kotlin/releases/download}"

stdlib_complete() {
  local stdlib="$1/klib/common/stdlib/default"
  [ -s "$stdlib/manifest" ] &&
    [ -d "$stdlib/linkdata" ] &&
    [ -n "$(find "$stdlib/linkdata" -type f -name '*.knm' -size +0 -print -quit)" ]
}

if stdlib_complete "$root"; then
  echo "$root"
  exit 0
fi
if [ -e "$root" ]; then
  echo "discarding incomplete kotlin-native ${ver} cache at $root…" >&2
  rm -rf "$root"
fi
url="$releases/v${ver}/${name}.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "downloading kotlin-native ${ver} for ${host}…" >&2
curl -fsSL "$url" -o "$tmp/kotlin-native.tar.gz"
tar -xzf "$tmp/kotlin-native.tar.gz" -C "$tmp"
stdlib_complete "$tmp/$name" || {
  echo "downloaded Kotlin/Native archive has no common stdlib KLIB metadata" >&2
  exit 1
}
mkdir -p "$parent"
mv "$tmp/$name" "$root"
echo "$root"
