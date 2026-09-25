#!/usr/bin/env bash
# Run the native codegen/box lane of one prebuilt conformance binary, partitioned into fresh
# processes that each receive the canonical conformance deadline. The lane compiles, links and RUNS
# every accepted case, so it grows with what the native backend accepts; the gate's rule is that a
# workload past the ceiling is divided, never exempted.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <conformance-bin>" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$script_dir/test-gate-defaults.sh"
source "$script_dir/test-deadline.sh"
source "$script_dir/libtest-shards.sh"

bin="$1"
[ -x "$bin" ] || { echo "conformance binary is not executable: $bin" >&2; exit 1; }
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"

shards="$KRUSTY_NATIVE_CONFORMANCE_SHARDS"
libtest_require_positive_shard_count \
  "$shards" "native-conformance-run: KRUSTY_NATIVE_CONFORMANCE_SHARDS"

for ((shard = 0; shard < shards; shard++)); do
  set +e
  KRUSTY_NATIVE_CONFORMANCE_SHARD_INDEX="$shard" \
    KRUSTY_NATIVE_CONFORMANCE_SHARD_COUNT="$shards" \
    run_with_deadline "$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS" \
    "$bin" --exact kotlin_box_native_conformance::kotlin_codegen_box_native_conformance \
    --nocapture >&2
  status=$?
  set -e
  if [ "$status" -eq 124 ]; then
    echo "native-conformance-run: timed out after ${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS}s: shard $((shard + 1))/$shards" >&2
  fi
  if [ "$status" -ne 0 ]; then
    exit "$status"
  fi
done
