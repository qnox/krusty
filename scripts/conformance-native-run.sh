#!/usr/bin/env bash
# Run the native lane of a prebuilt conformance binary against one Kotlin version's box corpus, with
# the canonical process-group deadline. Needs no kotlinc: the native lane's oracle is `box()`
# printing `OK`, and the corpus alone supplies the cases.
#
# The corpus is PARTITIONED across shards, one process each, because the whole lane outgrew the
# two-minute deadline and `scripts/test-gate-defaults.sh` says such a workload is divided rather
# than granted an exception. Each shard scans its own slice and applies the expected-failure ledger
# to the cases it scanned, so the gate — every accepted case prints `OK`, and no ledger entry passes
# — holds shard by shard and therefore over the whole corpus.
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
box_dir="${KRUSTY_KOTLIN_BOX_DIR:-$(just --justfile "$repo_root/justfile" box-corpus "$v")}"
export KRUSTY_LANGUAGE_VERSION="$v"
export KRUSTY_KOTLIN_BOX_DIR="$box_dir"
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"

shards="$KRUSTY_CONFORMANCE_SHARDS"
libtest_require_positive_shard_count \
  "$shards" "conformance-native-run: KRUSTY_CONFORMANCE_SHARDS"

report_dir="$(mktemp -d)"
trap 'rm -rf "$report_dir"' EXIT
for ((shard = 0; shard < shards; shard++)); do
  report="$report_dir/shard-$shard.report"
  set +e
  KRUSTY_NATIVE_CONFORMANCE_SHARD_INDEX="$shard" \
    KRUSTY_NATIVE_CONFORMANCE_SHARD_COUNT="$shards" \
    KRUSTY_NATIVE_CONFORMANCE_REPORT="$report" \
    run_with_deadline "$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS" \
    "$bin" kotlin_codegen_box_native_conformance --nocapture >&2
  status=$?
  set -e
  if [ "$status" -eq 124 ]; then
    echo "conformance-native-run: timed out after ${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS}s: Kotlin $v, shard $((shard + 1))/$shards" >&2
  fi
  if [ "$status" -ne 0 ]; then
    exit "$status"
  fi
  [ -s "$report" ] || {
    echo "native conformance test did not write its report: shard $((shard + 1))/$shards" >&2
    exit 1
  }
done

# One line summing the shards, so the artifact reads like the single-process report it replaces,
# followed by each shard's own report and backlog tables.
awk -v shards="$shards" '
  /^native conformance:/ {
    line = $0
    sub(/^native conformance: /, "", line)
    sub(/ \(.*$/, "", line)
    n = split(line, fields, ", ")
    for (i = 1; i <= n; i++) {
      split(fields[i], kv, " ")
      key = kv[1]
      if (!(key in seen)) { order[++keys] = key; seen[key] = 1 }
      total[key] += kv[2]
    }
    next
  }
  END {
    out = ""
    for (i = 1; i <= keys; i++) {
      out = out (i == 1 ? "" : ", ") order[i] " " total[order[i]]
    }
    printf "native conformance: %s (%d shards)\n", out, shards
  }
' "$report_dir"/shard-*.report

cat "$report_dir"/shard-*.report
