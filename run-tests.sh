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

# Filtered/profile-specific runs are single-purpose; defer to cargo's normal runner. They may not need
# a JVM at all, so the toolchain preflight below deliberately sits after this and guards only the full
# suite; a filtered run that does need one still gets the same diagnosis from `common::jdk_modules`.
if [ "$#" -ne 0 ] || [ "$profile_overridden" -ne 0 ]; then
  exec cargo test $profile_arg "$@"
fi

# Hundreds of e2e tests resolve `java.*` against the JDK's `lib/modules` jimage. Without it they all
# fail identically, minutes into the run, with a bare `.expect` panic and no hint of the cause — which
# reads exactly like a mass regression. Stop before building instead. `just test` and the lefthook
# pre-push gate both land here, so a shell without JAVA_HOME is caught before it can waste a full run.
if [ -n "${KRUSTY_SURVEY_JDK_MODULES:-}" ]; then
  if [ ! -f "${KRUSTY_SURVEY_JDK_MODULES}" ]; then
    echo "run-tests.sh: KRUSTY_SURVEY_JDK_MODULES is set but is not a file:" >&2
    echo "  ${KRUSTY_SURVEY_JDK_MODULES}" >&2
    echo "It must point at a JDK's lib/modules jimage. Unset it to fall back to JAVA_HOME." >&2
    exit 2
  fi
else
  # Matches the precedence in `krusty::toolchain::jdk_modules`.
  jdk_home="${JAVA_HOME:-${KRUSTY_REF_JAVA_HOME:-}}"
  if [ -z "${jdk_home}" ]; then
    echo "run-tests.sh: JAVA_HOME is not set, so the JVM-backed tests cannot run." >&2
    echo "There is no fallback to /usr/libexec/java_home — the variable must be set explicitly." >&2
    if [ "$(uname -s)" = "Darwin" ] && [ -x /usr/libexec/java_home ]; then
      echo "Try: JAVA_HOME=\"\$(/usr/libexec/java_home -v 21)\" $0" >&2
    else
      echo "Set it to a JDK 21+ home and re-run." >&2
    fi
    exit 2
  fi
  if [ ! -f "${jdk_home}/lib/modules" ]; then
    echo "run-tests.sh: no lib/modules jimage under JAVA_HOME:" >&2
    echo "  ${jdk_home}" >&2
    echo "That path is not a JDK home. A package-manager prefix is the usual mistake — the real home" >&2
    echo "is often a subdirectory (e.g. .../openjdk@21/libexec/openjdk.jdk/Contents/Home)." >&2
    exit 2
  fi
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

# `$2` is `<binary>[::<extra args>]`; optional `$3` labels this invocation. The label matters when one
# binary is invoked more than once (the conformance binary is run twice, below): the log name derives
# from the binary's basename, so without a distinct label the second run TRUNCATES the first run's log
# and a failure in the first gets reported with the second's passing output — a FAILED binary whose
# printed log shows zero failures. FAILED therefore records the log key next to the display name, and
# each labelled invocation gets its own TIMINGS row (they are separate processes with separate wall
# times, so one row per run is the honest report).
run_one() {
  local b="${2%%::*}" extra="" name key
  [ "$2" != "$b" ] && extra="${2#*::}"
  name="$(basename "$b")"
  key="$name${3:+.$3}"
  local start end ms
  start="$(epoch_ms)"
  if "$b" $extra >"$1/$key.log" 2>&1; then
    :
  else
    printf '%s\t%s\n' "$key" "$b${3:+ ($3)}" >>"$1/FAILED"
  fi
  end="$(epoch_ms)"
  ms=$((end - start))
  printf '%08d %s\n' "$ms" "$key" >>"$1/TIMINGS"
}
export -f run_one

# The conformance binary contains external corpus/reference-toolchain suites. Run it alone before
# the product test binary to avoid core contention and to keep fast/coverage exclusion binary-scoped.
# The Kotlin codegen corpus test is memory-heavy, so run it in its own process, then run every other
# conformance test in a fresh process. This still executes the full conformance binary's test set; it
# just avoids carrying earlier external-suite state into the large corpus pass on small CI machines.
gate="$(printf '%s\n' "${bins[@]}" | grep '/conformance-' || true)"
if [ -n "$gate" ]; then
  run_one "$logdir" "$gate::kotlin_codegen_box_conformance --test-threads=1" box-corpus
  run_one "$logdir" "$gate::--skip kotlin_codegen_box_conformance --test-threads=1" rest
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
  while IFS=$'\t' read -r key label; do
    echo "----- $label -----"
    cat "$logdir/$key.log"
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
