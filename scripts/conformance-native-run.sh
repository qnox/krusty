#!/usr/bin/env bash
# Run the native lane of a prebuilt conformance binary against one Kotlin version's box corpus, with
# the canonical process-group deadline. Needs no kotlinc: the native lane's oracle is `box()`
# printing `OK`, and the corpus alone supplies the cases.
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
box_dir="${KRUSTY_KOTLIN_BOX_DIR:-$(just --justfile "$repo_root/justfile" box-corpus "$v")}"
export KRUSTY_LANGUAGE_VERSION="$v"
export KRUSTY_KOTLIN_BOX_DIR="$box_dir"
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"

report="$(mktemp)"
trap 'rm -f "$report"' EXIT
set +e
KRUSTY_NATIVE_CONFORMANCE_REPORT="$report" \
  run_with_deadline "$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS" \
  "$bin" kotlin_codegen_box_native_conformance --nocapture >&2
status=$?
set -e
if [ "$status" -eq 124 ]; then
  echo "conformance-native-run: timed out after ${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS}s: Kotlin $v" >&2
fi
if [ "$status" -ne 0 ]; then
  exit "$status"
fi
[ -s "$report" ] || { echo "native conformance test did not write its report" >&2; exit 1; }
cat "$report"
