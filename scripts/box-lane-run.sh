#!/usr/bin/env bash
# Run one runnable-target codegen/box lane (native, wasm-js or wasm-wasi) of one prebuilt
# conformance binary as one process. The suite has a dedicated configurable deadline because it
# compiles and runs every accepted case. Its score is the same `<pct> <passed> <applicable>` machine
# format as JVM conformance.
set -euo pipefail

if [ "$#" -ne 2 ] && [ "$#" -ne 3 ]; then
  echo "usage: $0 <native|wasm-js|wasm-wasi> <conformance-bin> [conformance-report]" >&2
  exit 2
fi

lane="$1"
shift
case "$lane" in
  native) infix=NATIVE test=kotlin_box_native_conformance::kotlin_codegen_box_native_conformance ;;
  wasm-js) infix=WASM_JS test=kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_js_conformance ;;
  wasm-wasi) infix=WASM_WASI test=kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_wasi_conformance ;;
  *) echo "unknown box lane: $lane" >&2; exit 2 ;;
esac

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$script_dir/test-gate-defaults.sh"
source "$script_dir/test-deadline.sh"
source "$script_dir/conformance-report.sh"

bin="$1"
report_out="${2:-}"
[ -x "$bin" ] || { echo "conformance binary is not executable: $bin" >&2; exit 1; }
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"
export "KRUSTY_REQUIRE_${infix}_CONFORMANCE=1"
if [ -z "${KRUSTY_TEST_THREADS:-}" ]; then
  lane_threads="$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu)"
  [ "$lane_threads" -le 4 ] || lane_threads=4
  export KRUSTY_TEST_THREADS="$lane_threads"
fi

report="$(mktemp)"
trap 'rm -f "$report"' EXIT
# A run that stops before producing totals must not leave an earlier caller-owned report behind.
[ -z "$report_out" ] || : >"$report_out"

export "KRUSTY_${infix}_CONFORMANCE_REPORT=$report"
set +e
run_with_deadline "$KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS" \
  "$bin" --exact "$test" --nocapture >&2
status=$?
set -e
if [ "$status" -eq 124 ]; then
  echo "$lane-conformance-run: timed out after ${KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS}s while running $lane box conformance" >&2
  exit "$status"
fi

parsed="$(conformance_report_parse "$report")" || {
  echo "$lane-conformance-run: test did not write a valid \"<pct> <passed> <applicable>\" report" >&2
  exit $((status == 0 ? 1 : status))
}
read -r pct passed applicable <<<"$parsed"
expected="$(conformance_report_line "$passed" "$applicable")"
[ "$parsed" = "$expected" ] || {
  echo "$lane-conformance-run: percentage $pct% does not match $passed/$applicable cases" >&2
  exit 1
}
[ -z "$report_out" ] || printf '%s\n' "$parsed" >"$report_out"
echo "$lane-conformance-run: Kotlin ${KRUSTY_LANGUAGE_VERSION:-unknown} $lane conformance: $parsed" >&2
exit "$status"
