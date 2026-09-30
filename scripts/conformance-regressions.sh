#!/usr/bin/env bash
# Run every test in one prebuilt conformance binary except the box corpus.
# The box suite stays on conformance-run.sh, once per supported Kotlin version.
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

# Provision only when the caller has not already pointed at a compiler. A test
# supplies KRUSTY_KOTLINC so this path stays off the network.
if [ -z "${KRUSTY_KOTLINC:-}" ]; then
  KRUSTY_KOTLINC="$(just --justfile "$repo_root/justfile" kotlinc "$v")"
  export KRUSTY_KOTLINC
  just --justfile "$repo_root/justfile" box-corpus "$v" >/dev/null
  just --justfile "$repo_root/justfile" ser-corpus "$v" >/dev/null
fi
export KRUSTY_LANGUAGE_VERSION="$v"
if [ -z "${KRUSTY_KOTLIN_BOX_DIR:-}" ]; then
  export KRUSTY_KOTLIN_BOX_DIR="$repo_root/target/cache/box-corpus/$v/compiler/testData/codegen/box"
fi
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"

# The downloaded test binary is not under target/<profile>/deps, so the suite's
# CLI lookup would rebuild krusty. A sibling built with the binary is that CLI.
if [ -z "${KRUSTY_BIN:-}" ]; then
  sibling="$(cd "$(dirname "$bin")" && pwd)/krusty"
  if [ -x "$sibling" ]; then
    export KRUSTY_BIN="$sibling"
  fi
fi

threads="$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
[ "$threads" -gt 4 ] && threads=4

set +e
run_with_deadline "$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS" \
  "$bin" --skip kotlin_codegen_box_conformance --test-threads "$threads"
status=$?
set -e
if [ "$status" -eq 124 ]; then
  echo "conformance-regressions: timed out after ${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS}s: Kotlin $v" >&2
fi
exit "$status"
