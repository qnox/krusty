#!/usr/bin/env bash
# Run one prebuilt conformance binary with the canonical process-group deadline and print the box
# byte-equality report "<pct> <matched> <total>" (see conformance-report.sh) summed over every shard.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <conformance-bin> <kotlin-version>" >&2
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
[ -x "$bin" ] || { echo "conformance binary is not executable: $bin" >&2; exit 1; }
kotlinc="${KRUSTY_KOTLINC:-$(just --justfile "$repo_root/justfile" kotlinc "$v")}"
box_dir="${KRUSTY_KOTLIN_BOX_DIR:-$(just --justfile "$repo_root/justfile" box-corpus "$v")}"
export KRUSTY_LANGUAGE_VERSION="$v"
export KRUSTY_KOTLINC="$kotlinc"
export KRUSTY_KOTLIN_BOX_DIR="$box_dir"
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"

# This runner always scores (it sets KRUSTY_CONFORMANCE_REPORT per shard), so it partitions by the
# scored-run shard count — finer than the plain gate's, to keep each reference-compiling shard
# inside the per-shard deadline.
shards="$KRUSTY_SCORED_CONFORMANCE_SHARDS"
libtest_require_positive_shard_count \
  "$shards" "conformance-run: KRUSTY_SCORED_CONFORMANCE_SHARDS"

report_dir="$(mktemp -d)"
trap 'phase_report || true; rm -rf "$report_dir" "$PHASE_TIMING_LOG"' EXIT
# Integer byte counts are summed across shards; the percentage is derived once, from the sums, so
# shards of unequal size are weighted by their bytes rather than averaged.
matched=0
total=0
# A shard that fails its expected-failure check still writes its report: keep running the remaining
# shards so one run lists every mismatch, then exit with the first failing status.
failed=0
for ((shard = 0; shard < shards; shard++)); do
  report="$report_dir/shard-$shard.report"
  label="box-shard-$((shard + 1))-of-$shards"
  phase_begin "$label"
  set +e
  KRUSTY_CONFORMANCE_SHARD_INDEX="$shard" \
    KRUSTY_CONFORMANCE_SHARD_COUNT="$shards" \
    KRUSTY_CONFORMANCE_REPORT="$report" \
    run_with_deadline "$KRUSTY_SCORED_CONFORMANCE_TIMEOUT_SECONDS" \
    "$bin" kotlin_codegen_box_conformance --nocapture >&2
  status=$?
  set -e
  phase_end "$label"
  if [ "$status" -eq 124 ]; then
    echo "conformance-run: timed out after ${KRUSTY_SCORED_CONFORMANCE_TIMEOUT_SECONDS}s: Kotlin $v, shard $((shard + 1))/$shards" >&2
    exit "$status"
  fi
  if [ "$status" -ne 0 ] && [ "$failed" -eq 0 ]; then
    failed="$status"
  fi
  [ -s "$report" ] || {
    echo "conformance test did not write its report: shard $((shard + 1))/$shards" >&2
    exit $((failed ? failed : 1))
  }
  parsed="$(conformance_report_parse "$report")" || {
    echo "conformance test wrote an invalid byte report (want \"<pct> <matched> <total>\" with matched <= total): shard $((shard + 1))/$shards" >&2
    exit 1
  }
  read -r _pct shard_matched shard_total <<<"$parsed"
  matched=$((matched + shard_matched))
  total=$((total + shard_total))
done

conformance_report_line "$matched" "$total"
exit "$failed"
