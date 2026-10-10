#!/usr/bin/env bash
# Run one prebuilt conformance binary with the canonical process-group deadline. The process writes
# two reports over the same applicable box cases (see conformance-report.sh). stdout is the case
# report "<pct> <passed> <applicable>". The JVM byte report
# "<pct> <matched> <total>" goes to stderr and, when given, to the file jvm-byte-report.
set -euo pipefail

if [ "$#" -ne 2 ] && [ "$#" -ne 3 ]; then
  echo "usage: $0 <conformance-bin> <kotlin-version> [jvm-byte-report]" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
source "$script_dir/test-gate-defaults.sh"
source "$script_dir/test-deadline.sh"
source "$script_dir/conformance-report.sh"
source "$script_dir/phase-timing.sh"
phase_log_prefix=conformance-run
export phase_log_prefix
PHASE_TIMING_LOG="$(mktemp)"

bin="$1"
v="$2"
byte_out="${3:-}"
[ -x "$bin" ] || { echo "conformance binary is not executable: $bin" >&2; exit 1; }
# Truncate first: a run that stops before its totals must not leave an earlier run's report behind.
[ -z "$byte_out" ] || : >"$byte_out"
kotlinc="${KRUSTY_KOTLINC:-$(just --justfile "$repo_root/justfile" kotlinc "$v")}"
box_dir="${KRUSTY_KOTLIN_BOX_DIR:-$(just --justfile "$repo_root/justfile" box-corpus "$v")}"
export KRUSTY_LANGUAGE_VERSION="$v"
export KRUSTY_KOTLINC="$kotlinc"
export KRUSTY_KOTLIN_BOX_DIR="$box_dir"
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"

report_dir="$(mktemp -d)"
trap 'phase_report || true; rm -rf "$report_dir" "$PHASE_TIMING_LOG"' EXIT
case_report="$report_dir/cases"
byte_report="$report_dir/jvm-bytes"
# Validate report $3 and print its "<count> <of>"; $1 names the report and $2 its line shape.
report_counts() {
  local parsed
  [ -s "$3" ] || {
    echo "conformance test did not write its $1" >&2
    exit $((status ? status : 1))
  }
  parsed="$(conformance_report_parse "$3")" || {
    echo "conformance test wrote an invalid $1 (want $2)" >&2
    exit 1
  }
  printf '%s\n' "${parsed#* }"
}

phase_begin box-conformance
set +e
KRUSTY_CONFORMANCE_REPORT="$case_report" \
  KRUSTY_JVM_BYTE_REPORT="$byte_report" \
  run_with_deadline "$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS" \
  "$bin" kotlin_codegen_box_conformance --nocapture >&2
status=$?
set -e
phase_end box-conformance
if [ "$status" -eq 124 ]; then
  echo "conformance-run: timed out after ${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS}s: Kotlin $v" >&2
  exit "$status"
fi

counts="$(report_counts "case report" \
  '"<pct> <passed> <applicable>" with passed <= applicable' "$case_report")" || exit $?
read -r passed applicable <<<"$counts"
counts="$(report_counts "JVM byte report" \
  '"<pct> <matched> <total>" with matched <= total' "$byte_report")" || exit $?
read -r matched total <<<"$counts"

bytes="$(conformance_report_line "$matched" "$total")"
[ -z "$byte_out" ] || printf '%s\n' "$bytes" >"$byte_out"
echo "conformance-run: Kotlin $v JVM byte equality (matched/total .class bytes): $bytes" >&2
conformance_report_line "$passed" "$applicable"
exit "$status"
