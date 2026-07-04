#!/usr/bin/env bash
# Canonical test runner for krusty. Use only this script to run the suite.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

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
cargo test --profile gate --no-run 2>&1 | tee "$build_log"

bins=()
while IFS=$'\t' read -r target path; do
  case "$target" in
    "unittests src/main.rs"|"unittests src/bin/"*) continue ;;
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
# Per-binary test threads. Keep the default at 1 because many formerly separate e2e files now
# share one process; callers can raise KRUSTY_TEST_THREADS after checking for temp-dir/state races.
threads="${KRUSTY_TEST_THREADS:-1}"
rest=()
while IFS= read -r b; do
  rest+=("$b")
done < <(printf '%s\n' "${bins[@]}" | grep -v '/conformance-')

# Long-running binaries first: otherwise alphabetical order leaves slow binaries to start late, creating
# a long tail on 4-core machines.
priority=(
  e2e
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

if [ "${#ordered[@]}" -gt 0 ]; then
  printf '%s\n' "${ordered[@]}" \
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
