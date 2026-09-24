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
source "$script_dir/libtest-shards.sh"

bin="$1"
v="$2"
[ -x "$bin" ] || { echo "conformance binary is not executable: $bin" >&2; exit 1; }
kotlinc="${KRUSTY_KOTLINC:-$(just --justfile "$repo_root/justfile" kotlinc "$v")}"
box_dir="${KRUSTY_KOTLIN_BOX_DIR:-$(just --justfile "$repo_root/justfile" box-corpus "$v")}"
export KRUSTY_LANGUAGE_VERSION="$v"
export KRUSTY_KOTLINC="$kotlinc"
export KRUSTY_KOTLIN_BOX_DIR="$box_dir"
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"

shards="$KRUSTY_CONFORMANCE_SHARDS"
libtest_require_positive_shard_count \
  "$shards" "conformance-run: KRUSTY_CONFORMANCE_SHARDS"

report_dir="$(mktemp -d)"
trap 'rm -rf "$report_dir"' EXIT
passed=0
scanned=0
# A shard that fails its expected-failure check still writes its report: keep running the remaining
# shards so one run lists every mismatch, then exit with the first failing status.
failed=0
for ((shard = 0; shard < shards; shard++)); do
  report="$report_dir/shard-$shard.report"
  set +e
  KRUSTY_CONFORMANCE_SHARD_INDEX="$shard" \
    KRUSTY_CONFORMANCE_SHARD_COUNT="$shards" \
    KRUSTY_CONFORMANCE_REPORT="$report" \
    run_with_deadline "$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS" \
    "$bin" kotlin_codegen_box_conformance --nocapture >&2
  status=$?
  set -e
  if [ "$status" -eq 124 ]; then
    echo "conformance-run: timed out after ${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS}s: Kotlin $v, shard $((shard + 1))/$shards" >&2
    exit "$status"
  fi
  if [ "$status" -ne 0 ] && [ "$failed" -eq 0 ]; then
    failed="$status"
  fi
  [ -s "$report" ] || {
    echo "conformance test did not write its report: shard $((shard + 1))/$shards" >&2
    exit $((failed ? failed : 1))
  }
  read -r _pct shard_passed shard_scanned extra <"$report"
  case "$shard_passed:$shard_scanned" in
    *[!0-9:]* | *::* | :* | *:)
      echo "conformance test wrote an invalid report: shard $((shard + 1))/$shards" >&2
      exit 1
      ;;
  esac
  if [ -n "${extra:-}" ]; then
    echo "conformance test wrote an invalid report: shard $((shard + 1))/$shards" >&2
    exit 1
  fi
  passed=$((passed + shard_passed))
  scanned=$((scanned + shard_scanned))
done

awk -v passed="$passed" -v scanned="$scanned" 'BEGIN {
  pct = scanned == 0 ? 0 : 100 * passed / scanned
  printf "%.1f %d %d\n", pct, passed, scanned
}'
exit "$failed"
