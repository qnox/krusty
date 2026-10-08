#!/usr/bin/env bash
# Run the Native codegen/box lane of one prebuilt conformance binary as one process. The suite has
# a dedicated configurable deadline because it compiles, links and runs every accepted case. Its
# score is the same `<pct> <passed> <applicable>` machine format as JVM conformance.
set -euo pipefail

if [ "$#" -ne 1 ] && [ "$#" -ne 2 ]; then
  echo "usage: $0 <conformance-bin> [native-conformance-report]" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$script_dir/test-gate-defaults.sh"
source "$script_dir/test-deadline.sh"
source "$script_dir/conformance-report.sh"

bin="$1"
report_out="${2:-}"
[ -x "$bin" ] || { echo "conformance binary is not executable: $bin" >&2; exit 1; }
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"
export KRUSTY_REQUIRE_NATIVE_CONFORMANCE=1
if [ -z "${KRUSTY_TEST_THREADS:-}" ]; then
  native_threads="$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu)"
  [ "$native_threads" -le 4 ] || native_threads=4
  export KRUSTY_TEST_THREADS="$native_threads"
fi

report="$(mktemp)"
trap 'rm -f "$report"' EXIT
# A run that stops before producing totals must not leave an earlier caller-owned report behind.
[ -z "$report_out" ] || : >"$report_out"

set +e
KRUSTY_NATIVE_CONFORMANCE_REPORT="$report" \
  run_with_deadline "$KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS" \
  "$bin" --exact kotlin_box_native_conformance::kotlin_codegen_box_native_conformance \
  --nocapture >&2
status=$?
set -e
if [ "$status" -eq 124 ]; then
  echo "native-conformance-run: timed out after ${KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS}s while running native box conformance" >&2
  exit "$status"
fi

parsed="$(conformance_report_parse "$report")" || {
  echo 'native-conformance-run: test did not write a valid "<pct> <passed> <applicable>" report' >&2
  exit $((status == 0 ? 1 : status))
}
read -r pct passed applicable <<<"$parsed"
expected="$(conformance_report_line "$passed" "$applicable")"
[ "$parsed" = "$expected" ] || {
  echo "native-conformance-run: percentage $pct% does not match $passed/$applicable cases" >&2
  exit 1
}
[ -z "$report_out" ] || printf '%s\n' "$parsed" >"$report_out"
echo "native-conformance-run: Kotlin ${KRUSTY_LANGUAGE_VERSION:-unknown} Native conformance: $parsed" >&2
exit "$status"
