#!/usr/bin/env bash
# Run the diagnostics test data lane (tests/kotlin_diagnostics_jvm_conformance.rs) of one prebuilt
# conformance binary for one Kotlin version. Ordinary runs ignore the lane because a full run
# compiles every case; this runs it explicitly under KRUSTY_DIAGNOSTICS_TIMEOUT_SECONDS.
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

if [ -z "${KRUSTY_KOTLINC:-}" ]; then
  KRUSTY_KOTLINC="$(just --justfile "$repo_root/justfile" kotlinc "$v")"
  export KRUSTY_KOTLINC
fi
# The lane finds the JetBrains checkout through the box corpus directory.
if [ -z "${KRUSTY_KOTLIN_BOX_DIR:-}" ]; then
  KRUSTY_KOTLIN_BOX_DIR="$(just --justfile "$repo_root/justfile" box-corpus "$v")"
  export KRUSTY_KOTLIN_BOX_DIR
fi
export KRUSTY_LANGUAGE_VERSION="$v"
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"

# The downloaded test binary is not under target/<profile>/deps, so the suite's CLI lookup would
# rebuild krusty. A sibling built with the binary is that CLI.
if [ -z "${KRUSTY_BIN:-}" ]; then
  sibling="$(cd "$(dirname "$bin")" && pwd)/krusty"
  if [ -x "$sibling" ]; then
    export KRUSTY_BIN="$sibling"
  fi
fi

set +e
run_with_deadline "$KRUSTY_DIAGNOSTICS_TIMEOUT_SECONDS" \
  "$bin" \
  --ignored \
  --exact kotlin_diagnostics_jvm_conformance::krusty_rejects_what_kotlinc_rejects_in_its_diagnostics_test_data \
  --nocapture
status=$?
set -e
if [ "$status" -eq 124 ]; then
  echo "diagnostics-conformance: timed out after ${KRUSTY_DIAGNOSTICS_TIMEOUT_SECONDS}s: Kotlin $v" >&2
fi
exit "$status"
