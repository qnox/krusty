#!/usr/bin/env bash
# Canonical test runner for krusty. Use only this script to run the suite.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

cd "$(dirname "$0")"

if command -v just >/dev/null 2>&1; then
  v="$(just max-version)"
  export KRUSTY_KOTLINC="${KRUSTY_KOTLINC:-$(just kotlinc "$v")}"
  export KRUSTY_KOTLIN_BOX_DIR="${KRUSTY_KOTLIN_BOX_DIR:-$(just box-corpus "$v")}"
fi

# Default to the fast-iteration `gate` profile (unoptimized → seconds-long rebuilds, but with
# overflow-checks/assertions off so krusty's wrapping arithmetic doesn't abort). The in-loop tests don't
# need optimization; release builds make the edit/build/test cycle slower overall.
profile_arg="--profile gate"
profile_overridden=0
for a in "$@"; do
  case "$a" in
    --release)
      echo "run-tests.sh uses the gate profile; --release slows the build/test cycle overall." >&2
      exit 2
      ;;
    --profile|--profile=*) profile_arg=""; profile_overridden=1 ;;
  esac
done

# Filtered/profile-specific runs are single-purpose; defer to cargo's normal runner.
if [ "$#" -ne 0 ] || [ "$profile_overridden" -ne 0 ]; then
  exec cargo test $profile_arg "$@"
fi

logdir="$(mktemp -d)"
cleanup() { rm -rf "$logdir"; }
trap cleanup EXIT

# Full-suite harness: build once, then run test binaries in parallel. Plain `cargo test` runs each
# integration-test binary sequentially, which is slow for this repo because many binaries pay JVM
# startup/warmup costs. Running binaries concurrently keeps each binary's in-process shared JVM runner
# while avoiding the sequential binary bottleneck.
build_log="$logdir/build.log"
cargo test --profile gate --no-run 2>&1 | tee "$build_log"

bins=()
while IFS= read -r line; do
  bins+=("$line")
done < <(sed -nE 's/.*[Ee]xecutable [^(]*\(([^)]+)\)/\1/p' "$build_log" | sort -u)

run_one() {
  local b="${2%%::*}" extra="" name
  [ "$2" != "$b" ] && extra="${2#*::}"
  name="$(basename "$b")"
  local start end ms
  start="$(date +%s%3N)"
  if "$b" $extra >"$1/$name.log" 2>&1; then
    :
  else
    echo "$b" >>"$1/FAILED"
  fi
  end="$(date +%s%3N)"
  ms=$((end - start))
  printf '%08d %s\n' "$ms" "$name" >>"$1/TIMINGS"
}
export -f run_one

# The conformance gate is internally rayon-parallel, so run it alone before the parallel batch to avoid
# core contention with the rest of the suite.
gate="$(printf '%s\n' "${bins[@]}" | grep kotlin_box_ir_jvm_conformance || true)"
[ -n "$gate" ] && run_one "$logdir" "$gate"

jobs="${KRUSTY_TEST_JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu)}"
rest=()
while IFS= read -r b; do
  rest+=("$b")
done < <(printf '%s\n' "${bins[@]}" | grep -v kotlin_box_ir_jvm_conformance)

# Long-running binaries first: otherwise alphabetical order leaves slow binaries to start late, creating
# a long tail on 4-core machines.
priority=(
  serialization_krusty_only_e2e
  suspend_e2e
  bytecode_parity_e2e
  top_level_property_e2e
  classpath_receiver_lambda_e2e
  cli_dropin_e2e
  named_args_classpath_e2e
  classpath_default_args_e2e
  classpath_function_reference_e2e
  inline_splice_e2e
  diagnostics_match_kotlinc
  classreader_e2e
  codegen_host_e2e
  feature_box_e2e
)
ordered=()
for p in "${priority[@]}"; do
  for b in "${rest[@]}"; do
    case "$(basename "$b")" in
      "$p"-*) ordered+=("$b") ;;
    esac
  done
done
for b in "${rest[@]}"; do
  seen=0
  for o in "${ordered[@]}"; do
    [ "$b" = "$o" ] && { seen=1; break; }
  done
  [ "$seen" -eq 0 ] && ordered+=("$b")
done

printf '%s\n' "${ordered[@]}" \
  | xargs -P "$jobs" -I{} bash -c 'run_one "$0" "$1::--test-threads=1"' "$logdir" {}

if [ -f "$logdir/FAILED" ]; then
  echo "=== FAILED TEST BINARIES ==="
  while read -r b; do
    echo "----- $b -----"
    cat "$logdir/$(basename "$b").log"
  done <"$logdir/FAILED"
  exit 1
fi

echo "=== SLOWEST TEST BINARIES ==="
# awk limits to 20 rows (rather than `| head -20`): head closing the pipe early makes `sort` take
# SIGPIPE, which under `set -o pipefail` fails this cosmetic diagnostic — and thus the whole (green)
# run — with 141. Letting awk consume all of sort's output keeps the pipeline exit status 0.
sort -rn "$logdir/TIMINGS" | awk 'NR <= 20 {printf "%7.2fs  %s\n", $1 / 1000, $2}'
echo "all test binaries passed"
