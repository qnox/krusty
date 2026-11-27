#!/usr/bin/env bash
# Validate the intellij-community integration against the exact revision it targets.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
revision=1608e029bb2c308b5c5f81a91a3e3d55830eb804
patch="$root/tools/krusty-bazel/intellij/0001-krusty-backend.patch"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

# The three files the patch applies to, pinned by content: a revision never changes, so a copy
# whose SHA-256 matches is the file. They are kept in a directory the workflow caches, and a copy
# that is already there is not downloaded again.
cache="${KRUSTY_TOOLS_DIR:-$HOME/.cache/krusty-tools}/intellij-community/$revision"
declare -A sha256=(
  [build/jvm-rules/MODULE.bazel]=2b66f9fae88032d504d82a6bcee07ad156bc00a177c996f71d2deee76cd3817e
  [build/jvm-rules/rules/common-attrs.bzl]=0b2f925409b7716807c057e678f1ff126d2d784f6dd0f0f4a0975246695a4531
  [build/jvm-rules/rules/impl/compile.bzl]=04a232a661c124679b845eddf1db2b472a93501b33dfa138b2a01109966b9a61
)

has_pinned_content() {
  [ -f "$1" ] && [ "$(sha256sum "$1" | awk '{print $1}')" = "$2" ]
}

download() {
  local path="$1" expected="${sha256[$1]}" cached="$cache/$1"
  if ! has_pinned_content "$cached" "$expected"; then
    mkdir -p "$(dirname "$cached")"
    local part="$cached.part.$$"
    # GitHub answers a burst of requests with 429; curl retries it, so wait between attempts.
    curl --proto '=https' --tlsv1.2 -fsSL --retry 5 --retry-delay 10 --retry-all-errors \
      "https://raw.githubusercontent.com/JetBrains/intellij-community/$revision/$path" \
      -o "$part"
    if ! has_pinned_content "$part" "$expected"; then
      rm -f "$part"
      echo "check-intellij-patch: $path at $revision does not match its pinned SHA-256 $expected" >&2
      return 1
    fi
    mv "$part" "$cached"
  fi
  mkdir -p "$(dirname "$scratch/$path")"
  cp "$cached" "$scratch/$path"
}

download build/jvm-rules/MODULE.bazel
download build/jvm-rules/rules/common-attrs.bzl
download build/jvm-rules/rules/impl/compile.bzl

git -C "$scratch" init -q
git -C "$scratch" apply --check "$patch"
git -C "$scratch" apply "$patch"

compile="$scratch/build/jvm-rules/rules/impl/compile.bzl"
grep -Fxq '        (javaCount == 0 or len(srcs.kt) > 0) and' "$compile"
grep -Fxq '        not plugins.stubs_phase and' "$compile"
grep -Fxq '        not plugins.compile_phase.classpath' "$compile"
grep -Fxq '            mnemonic = "KrustyCompile",' "$compile"
grep -Fxq '            mnemonic = "JvmCompile",' "$compile"
