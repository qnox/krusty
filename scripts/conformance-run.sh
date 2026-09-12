#!/usr/bin/env bash
# Run one prebuilt conformance binary with the canonical process-group deadline.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <conformance-bin> <kotlin-version>" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
source "$script_dir/test-gate-defaults.sh"
source "$script_dir/test-deadline.sh"

bin="$1"
v="$2"
[ -x "$bin" ] || { echo "conformance binary is not executable: $bin" >&2; exit 1; }
kotlinc="${KRUSTY_KOTLINC:-$(just --justfile "$repo_root/justfile" kotlinc "$v")}"
box_dir="${KRUSTY_KOTLIN_BOX_DIR:-$(just --justfile "$repo_root/justfile" box-corpus "$v")}"
export KRUSTY_LANGUAGE_VERSION="$v"
export KRUSTY_KOTLINC="$kotlinc"
export KRUSTY_KOTLIN_BOX_DIR="$box_dir"
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"

report="$(mktemp)"
trap 'rm -f "$report"' EXIT
set +e
KRUSTY_CONFORMANCE_REPORT="$report" \
  run_with_deadline "$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS" \
  "$bin" kotlin_codegen_box_conformance --nocapture >&2
status=$?
set -e
if [ "$status" -eq 124 ]; then
  echo "conformance-run: timed out after ${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS}s: Kotlin $v" >&2
fi
if [ "$status" -ne 0 ]; then
  exit "$status"
fi
[ -s "$report" ] || { echo "conformance test did not write its report" >&2; exit 1; }
cat "$report"
