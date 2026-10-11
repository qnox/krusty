#!/usr/bin/env bash
# Apply bazel/intellij/*.patch to an intellij-community checkout and build one target with krusty.
#
# Kotlin-only, plugin-free jvm_library targets compile with the binary in KRUSTY_BINARY. Java
# sources, source jars, and build-supplied plugins stay on the project's JPS builder.
#
#   cargo build --profile gate -p krusty-cli --bin krusty
#   scripts/intellij-krusty.sh //platform/icons-api:icons-api
#
# KRUSTY_IJ_ROOT overrides the checkout (default: ~/external-projects/intellij-community).
# Extra arguments are passed to `bazel build` after the target.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
ij="${KRUSTY_IJ_ROOT:-$HOME/external-projects/intellij-community}"
binary="${KRUSTY_BINARY:-$root/target/gate/krusty}"
target="${1:-//platform/icons-api:icons-api}"
if [[ $# -gt 0 ]]; then
  shift
fi

if [[ ! -x "$binary" ]]; then
  echo "krusty binary is not executable: $binary" >&2
  echo "build it with: cargo build --profile gate -p krusty-cli --bin krusty" >&2
  exit 1
fi
if [[ ! -f "$ij/MODULE.bazel" ]]; then
  echo "$ij does not look like an intellij-community checkout" >&2
  exit 1
fi

shopt -s nullglob
patches=("$root"/bazel/intellij/*.patch)
if [[ ${#patches[@]} -eq 0 ]]; then
  echo "no patches in $root/bazel/intellij" >&2
  exit 1
fi
for patch in "${patches[@]}"; do
  if git -C "$ij" apply --check "$patch" 2>/dev/null; then
    git -C "$ij" apply "$patch"
    echo "applied $(basename "$patch")"
  elif git -C "$ij" apply --reverse --check "$patch" 2>/dev/null; then
    echo "already applied: $(basename "$patch")"
  else
    echo "patch does not apply: $patch" >&2
    git -C "$ij" apply --check "$patch" >&2 || true
    exit 1
  fi
done

cd "$ij"
exec bash ./bazel.cmd build "$target" --repo_env="KRUSTY_BINARY=$binary" "$@"
