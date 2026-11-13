#!/usr/bin/env bash
set -euo pipefail

version=2.5.0-RC
native_root="$(just kotlin-native "$version")"
compiler="$native_root/bin/kotlinc-native"
out="${1:?usage: rc-klib-stamp-probe.sh <evidence-dir>}"
mkdir -p "$out"
test -x "$compiler"
printf '%s\n' "$native_root" > "$out/native-root.txt"
grep -E '^(abi_version|compiler_version|metadata_version|language_features)=' \
  "$native_root/klib/common/stdlib/default/manifest" > "$out/stdlib-stamp.txt"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
printf 'package probe\nfun answer(): Int = 42\n' > "$tmp/Probe.kt"

for level in 2.0 2.1 2.2 2.3 2.4 2.5 2.6 2.7; do
  log="$out/language-$level.log"
  klib="$tmp/probe-$level.klib"
  if [[ "$level" == 2.0 || "$level" == 2.1 ]]; then
    if "$compiler" -Xmetadata-klib -language-version "$level" \
      -produce library -o "$klib" "$tmp/Probe.kt" >"$log" 2>&1; then
      echo "kotlinc-native $version unexpectedly accepted language level $level" >&2
      exit 1
    fi
    printf 'language=%s rejected\n' "$level" | tee -a "$out/results.txt"
    continue
  fi

  "$compiler" -Xmetadata-klib -language-version "$level" \
    -produce library -o "$klib" "$tmp/Probe.kt" >"$log" 2>&1
  if [[ -d "$klib" ]]; then
    if [[ -f "$klib/default/manifest" ]]; then
      manifest="$(cat "$klib/default/manifest")"
    else
      manifest="$(cat "$klib/manifest")"
    fi
  else
    manifest="$(unzip -p "$klib" default/manifest 2>/dev/null || unzip -p "$klib" manifest)"
  fi
  abi="$(sed -n 's/^abi_version=//p' <<<"$manifest")"
  metadata="$(sed -n 's/^metadata_version=//p' <<<"$manifest")"
  expected="$level.0"
  if [[ "$abi" != 2.5.0 || "$metadata" != "$expected" ]]; then
    echo "language $level stamped abi_version=$abi metadata_version=$metadata; expected 2.5.0/$expected" >&2
    exit 1
  fi
  printf 'language=%s abi_version=%s metadata_version=%s\n' "$level" "$abi" "$metadata" \
    | tee -a "$out/results.txt"
done
