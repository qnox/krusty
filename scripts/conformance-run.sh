#!/usr/bin/env bash
# Run one prebuilt conformance binary with the canonical process-group deadline. Every shard writes
# two reports over the same applicable box cases (see conformance-report.sh); each is summed over
# every shard. stdout is the case report "<pct> <passed> <applicable>". The JVM byte report
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
source "$script_dir/libtest-shards.sh"
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

# This runner always scores (it sets KRUSTY_JVM_BYTE_REPORT per shard), so it partitions by the
# scored-run shard count — finer than the plain gate's — while each shard keeps the plain gate's
# conformance deadline.
shards="$KRUSTY_SCORED_CONFORMANCE_SHARDS"
libtest_require_positive_shard_count \
  "$shards" "conformance-run: KRUSTY_SCORED_CONFORMANCE_SHARDS"

report_dir="$(mktemp -d)"
trap 'phase_report || true; rm -rf "$report_dir" "$PHASE_TIMING_LOG"' EXIT
# Integer counts are summed across shards; each percentage is derived once, from its sums, so
# shards of unequal size are weighted by their cases or bytes rather than averaged.
passed=0
applicable=0
matched=0
total=0
# Validate shard report $3 and print its "<count> <of>"; $1 names the report and $2 its line shape
# in diagnostics. A missing report keeps an earlier shard's failing status.
shard_counts() {
  local parsed
  [ -s "$3" ] || {
    echo "conformance test did not write its $1: shard $((shard + 1))/$shards" >&2
    exit $((failed ? failed : 1))
  }
  parsed="$(conformance_report_parse "$3")" || {
    echo "conformance test wrote an invalid $1 (want $2): shard $((shard + 1))/$shards" >&2
    exit 1
  }
  printf '%s\n' "${parsed#* }"
}
# A shard that fails its expected-failure check still writes its reports: keep running the
# remaining shards so one run lists every mismatch, then exit with the first failing status.
failed=0
for ((shard = 0; shard < shards; shard++)); do
  case_report="$report_dir/shard-$shard.cases"
  byte_report="$report_dir/shard-$shard.jvm-bytes"
  label="box-shard-$((shard + 1))-of-$shards"
  phase_begin "$label"
  set +e
  KRUSTY_CONFORMANCE_SHARD_INDEX="$shard" \
    KRUSTY_CONFORMANCE_SHARD_COUNT="$shards" \
    KRUSTY_CONFORMANCE_REPORT="$case_report" \
    KRUSTY_JVM_BYTE_REPORT="$byte_report" \
    run_with_deadline "$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS" \
    "$bin" kotlin_codegen_box_conformance --nocapture >&2
  status=$?
  set -e
  phase_end "$label"
  if [ "$status" -eq 124 ]; then
    echo "conformance-run: timed out after ${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS}s: Kotlin $v, shard $((shard + 1))/$shards" >&2
    exit "$status"
  fi
  if [ "$status" -ne 0 ] && [ "$failed" -eq 0 ]; then
    failed="$status"
  fi
  counts="$(shard_counts "case report" \
    '"<pct> <passed> <applicable>" with passed <= applicable' "$case_report")" || exit $?
  read -r shard_passed shard_applicable <<<"$counts"
  counts="$(shard_counts "JVM byte report" \
    '"<pct> <matched> <total>" with matched <= total' "$byte_report")" || exit $?
  read -r shard_matched shard_total <<<"$counts"
  passed=$((passed + shard_passed))
  applicable=$((applicable + shard_applicable))
  matched=$((matched + shard_matched))
  total=$((total + shard_total))
done

bytes="$(conformance_report_line "$matched" "$total")"
[ -z "$byte_out" ] || printf '%s\n' "$bytes" >"$byte_out"
echo "conformance-run: Kotlin $v JVM byte equality (matched/total .class bytes): $bytes" >&2
conformance_report_line "$passed" "$applicable"
exit "$failed"
