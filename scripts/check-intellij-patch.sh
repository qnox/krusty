#!/usr/bin/env bash
# Validate the intellij-community integration against the exact revision it targets.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
revision=1608e029bb2c308b5c5f81a91a3e3d55830eb804
patch="$root/tools/krusty-bazel/intellij/0001-krusty-backend.patch"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

download() {
  local path="$1"
  mkdir -p "$(dirname "$scratch/$path")"
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 \
    "https://raw.githubusercontent.com/JetBrains/intellij-community/$revision/$path" \
    -o "$scratch/$path"
}

download build/jvm-rules/MODULE.bazel
download build/jvm-rules/rules/common-attrs.bzl
download build/jvm-rules/rules/impl/compile.bzl

git -C "$scratch" init -q
git -C "$scratch" apply --check "$patch"
git -C "$scratch" apply "$patch"

compile="$scratch/build/jvm-rules/rules/impl/compile.bzl"
grep -Fxq '        not plugins.stubs_phase and' "$compile"
grep -Fxq '        not plugins.compile_phase.classpath' "$compile"
grep -Fxq '            mnemonic = "KrustyCompile",' "$compile"
grep -Fxq '            mnemonic = "JvmCompile",' "$compile"
grep -Fxq '        args.add("--kotlinc-arg", "-Xkotlin-reference-version=" + KRUSTY_KOTLIN_VERSION)' "$compile"
grep -Fxq '    kotlin_version = _KOTLIN_VERSION,' "$scratch/build/jvm-rules/MODULE.bazel"
