#!/usr/bin/env bash
# Canonical test runner for krusty. Use only this script to run the suite.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

# The `gate` profile is unoptimized, so a test thread's default (~2 MiB) stack can overflow
# NON-DETERMINISTICALLY on legitimate deep recursion (e.g. multi-level inline splicing) — a frame-layout
# shift is enough to tip a passing run into `stack overflow, aborting`, which fails the whole binary (and
# the pre-push gate). Give the test threads a generous stack, matching `scripts/coverage.sh`.
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}" # 128 MiB

cd "$(dirname "$0")"

if command -v just >/dev/null 2>&1; then
  v="$(just max-version)"
  just kotlinc "$v" >/dev/null
  just box-corpus "$v" >/dev/null
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
cargo build --color never --profile gate -p krusty-cli
target_root="${CARGO_TARGET_DIR:-$PWD/target}"
[[ "$target_root" = /* ]] || target_root="$PWD/$target_root"
cli_name="krusty"
[[ "${OS:-}" = "Windows_NT" ]] && cli_name="krusty.exe"
export KRUSTY_BIN="$target_root/gate/$cli_name"
[[ -x "$KRUSTY_BIN" ]] || { echo "run-tests.sh: compiler binary missing: $KRUSTY_BIN" >&2; exit 1; }
cargo test --workspace --color never --profile gate --no-run 2>&1 | tee "$build_log"

bins=()
while IFS=$'\t' read -r target path; do
  case "$target" in
    *"src/main.rs"|"unittests src/bin/"*) continue ;;
  esac
  bins+=("$path")
done < <(sed -nE 's/.*[Ee]xecutable ([^(]+) \(([^)]+)\)/\1\t\2/p' "$build_log" | sort -u)

# KRUSTY_TEST_EXCLUDE: comma-separated test-binary base names to skip (e.g. the slow external-corpus
# suites for the fast pre-push run). Matched against each binary's name with the cargo hash stripped.
# Used by `just test-fast`; empty by default so the normal gate runs everything.
if [ -n "${KRUSTY_TEST_EXCLUDE:-}" ]; then
  IFS=',' read -r -a _excl <<<"$KRUSTY_TEST_EXCLUDE"
  kept=()
  for b in "${bins[@]}"; do
    stem="$(basename "$b" | sed -E 's/-[0-9a-f]+$//')"
    skip=0
    for e in "${_excl[@]}"; do [ "$stem" = "$e" ] && skip=1 && break; done
    [ "$skip" -eq 0 ] && kept+=("$b")
  done
  bins=("${kept[@]}")
fi

if [ "${#bins[@]}" -eq 0 ]; then
  echo "run-tests.sh: no test binaries scheduled after build/filter" >&2
  exit 1
fi

# Portable epoch milliseconds. GNU `date +%s%3N` yields millis, but BSD/macOS `date` has no `%N` and
# emits a literal `N` (`1700000000N`), which would poison the `$((end - start))` arithmetic below and
# abort the whole run under `set -e`. Detect a non-numeric result and fall back to python3 (true millis)
# or whole-second precision — the value only feeds the cosmetic TIMINGS report, so coarser is fine.
epoch_ms() {
  local t
  t="$(date +%s%3N 2>/dev/null)"
  case "$t" in
    '' | *[!0-9]*)
      if command -v python3 >/dev/null 2>&1; then
        python3 -c 'import time; print(int(time.time()*1000))'
      else
        echo $(($(date +%s) * 1000))
      fi
      ;;
    *) printf '%s\n' "$t" ;;
  esac
}
export -f epoch_ms

run_one() {
  local b="${2%%::*}" extra="" name
  [ "$2" != "$b" ] && extra="${2#*::}"
  name="$(basename "$b")"
  local start end ms
  start="$(epoch_ms)"
  if "$b" $extra >"$1/$name.log" 2>&1; then
    :
  else
    echo "$b" >>"$1/FAILED"
  fi
  end="$(epoch_ms)"
  ms=$((end - start))
  printf '%08d %s\n' "$ms" "$name" >>"$1/TIMINGS"
}
export -f run_one

# The conformance binary contains external corpus/reference-toolchain suites. Run it alone before
# the product test binary to avoid core contention and to keep fast/coverage exclusion binary-scoped.
# The Kotlin codegen corpus test is memory-heavy, so run it in its own process, then run every other
# conformance test in a fresh process. This still executes the full conformance binary's test set; it
# just avoids carrying earlier external-suite state into the large corpus pass on small CI machines.
gate="$(printf '%s\n' "${bins[@]}" | grep '/conformance-' || true)"
if [ -n "$gate" ]; then
  run_one "$logdir" "$gate::kotlin_codegen_box_conformance --test-threads=1"
  run_one "$logdir" "$gate::--skip kotlin_codegen_box_conformance --test-threads=1"
fi

ncpu="$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
jobs="${KRUSTY_TEST_JOBS:-$ncpu}"
# Per-binary test threads for the SMALL binaries run in the cross-binary xargs pool: keep 1 so `-P jobs`
# parallelizes ACROSS those fast unit-style suites without each ALSO spawning `ncpu` threads and
# over-subscribing the cores.
threads="${KRUSTY_TEST_THREADS:-1}"

rest=()
while IFS= read -r b; do
  rest+=("$b")
done < <(printf '%s\n' "${bins[@]}" | grep -v '/conformance-')

# The e2e binary joins ~250 formerly-separate e2e tests, many of which drive the real kotlinc plus a
# persistent JVM box runner. Run it DEDICATED and SEQUENTIALLY — after conformance, before the small-binary
# pool — with `--test-threads=$ncpu` so its tests parallelize INTERNALLY across all cores, and size the
# per-process box-runner pool to match so `ncpu` in-flight `box()` calls don't queue on too few runners.
# Running it alone (outside the `-P jobs` fan-out) keeps it from over-subscribing while it owns the cores.
e2e_bin="$(printf '%s\n' "${rest[@]}" | grep '/e2e-' | head -1 || true)"
pool="${KRUSTY_BOX_RUNNER_POOL:-$ncpu}"
if [ -n "$e2e_bin" ]; then
  KRUSTY_BOX_RUNNER_POOL="$pool" run_one "$logdir" "$e2e_bin::--test-threads=$ncpu"
fi

# Everything except conformance and e2e — small suites parallelized across binaries.
pool_bins=()
while IFS= read -r b; do
  pool_bins+=("$b")
done < <(printf '%s\n' "${rest[@]}" | grep -v '/e2e-')

if [ "${#pool_bins[@]}" -gt 0 ]; then
  printf '%s\n' "${pool_bins[@]}" \
    | xargs -P "$jobs" -I{} bash -c 'run_one "$0" "$1::--test-threads='"$threads"'"' "$logdir" {}
fi

if [ -f "$logdir/FAILED" ]; then
  echo "=== FAILED TEST BINARIES ==="
  while read -r b; do
    echo "----- $b -----"
    cat "$logdir/$(basename "$b").log"
  done <"$logdir/FAILED"
  exit 1
fi

echo "=== SLOWEST TEST BINARIES ==="
if [ ! -f "$logdir/TIMINGS" ]; then
  echo "run-tests.sh: no test binaries ran; scheduled ${#bins[@]} binaries" >&2
  exit 1
fi
# awk limits to 20 rows (rather than `| head -20`): head closing the pipe early makes `sort` take
# SIGPIPE, which under `set -o pipefail` fails this cosmetic diagnostic — and thus the whole (green)
# run — with 141. Letting awk consume all of sort's output keeps the pipeline exit status 0.
sort -rn "$logdir/TIMINGS" | awk 'NR <= 20 {printf "%7.2fs  %s\n", $1 / 1000, $2}'
echo "all test binaries passed"
