#!/usr/bin/env bash
# Run every JVM-owned non-box test, including the shared multimodule set through its JVM runner, in
# one prebuilt conformance binary. Runnable-target modules are excluded wholesale: their focused
# tests require the exact Native/Wasm setup, and scripts/target-conformance-regressions.sh runs
# them plus the same shared set through the provisioned target runner in those matrix rows.
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
export KRUSTY_MULTIMODULE_CONFORMANCE_RUNNER=jvm
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

# nproc is GNU coreutils; getconf is POSIX (and on macOS lives in /usr/bin, which minimal PATHs
# keep, unlike /usr/sbin/sysctl). tests/test_harness_sharding_e2e.rs mirrors this probe verbatim in
# regression_threads() and asserts the resulting count — keep the two pipelines identical.
threads="$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu)"
[ "$threads" -gt 4 ] && threads=4

set +e
run_with_deadline "$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS" \
  "$bin" \
  --skip kotlin_codegen_box_conformance \
  --skip kotlin_box_native_conformance:: \
  --skip kotlin_box_wasm_conformance:: \
  --test-threads "$threads"
status=$?
set -e
if [ "$status" -eq 124 ]; then
  echo "conformance-regressions: timed out after ${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS}s: Kotlin $v" >&2
fi
[ "$status" -eq 0 ] || exit "$status"

# The CLI corpus ratchet is only as sound as its runner. The reference kotlinc runs through the
# same runner and must reproduce every case the not-applicable list does not excuse. Its kotlinc
# observations replay from the recorded-bytes archive; a case whose inputs changed runs kotlinc,
# which is the only way to observe it, so a missing recording compiles here as it does in CI.
# A cold archive takes minutes, so ordinary test runs ignore the test and this gate runs it.
set +e
KRUSTY_CLASS_DUMP_COMPILE_MISSING="${KRUSTY_CLASS_DUMP_COMPILE_MISSING:-1}" \
run_with_deadline "$KRUSTY_CLI_REFERENCE_TIMEOUT_SECONDS" \
  "$bin" \
  --ignored \
  --exact kotlin_cli_jvm_conformance::the_reference_compiler_reproduces_every_applicable_cli_case
status=$?
set -e
if [ "$status" -eq 124 ]; then
  echo "conformance-regressions: CLI corpus reference run timed out after ${KRUSTY_CLI_REFERENCE_TIMEOUT_SECONDS}s: Kotlin $v" >&2
fi
exit "$status"
