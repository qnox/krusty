#!/usr/bin/env bash
# Measure test coverage (regions, functions, lines, branches) via LLVM source-based coverage.
#
# Branch coverage needs the nightly `-Zcoverage-options=branch` path, so the whole run goes through
# the nightly toolchain. llvm-tools-preview (nightly) and cargo-llvm-cov must be installed.
#
# RUNNER — this deliberately does NOT use `cargo llvm-cov test` (runs binaries serially) nor
# `cargo llvm-cov nextest` (process-per-test; net-negative here — every test contends on the shared
# JVM daemon, and tests that share per-binary state fail under separate processes). It mirrors
# run-tests.sh: instrument via `llvm-cov show-env`, build once, run the selected test binaries, then
# aggregate the profraw counters into a report. Increase KRUSTY_TEST_JOBS explicitly for local
# experiments; CI defaults to the stable single-worker path.
#
# SCOPE — the metric reflects krusty's OWN test suite, not imported external suites. These are
# EXCLUDED: their INPUT is an external corpus or the reference compiler, so counting them would
# measure kotlinc's coverage of its own testdata. To exclude a new external suite, add it here.
EXCLUDE=(
  conformance   # external corpus/reference-toolchain suites (Kotlin box, serialization, KSP)
)

set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."

# The coverage build is UNoptimized (source-based instrumentation, no `--release`), so its stack
# frames are large. The deepest compiler paths — e.g. `inline_deep_coverage_e2e::inline_five_levels_deep`,
# which recurses through the inline-function splicer five levels down — sit right at libtest's default
# per-test thread stack (~2 MiB) and overflow it non-deterministically: an unrelated code change that
# shifts frame layout is enough to tip a passing run into a `stack overflow, aborting` (which fails the
# whole binary → the gate). Give the test threads a generous stack so legitimate deep recursion in the
# debug build never aborts. The optimized `run-tests.sh` path has small enough frames not to need this.
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}" # 128 MiB

summary_out="${1:-target/coverage/summary.json}"
compiler_raw_out="target/coverage/compiler-full.json"
lsp_raw_out="target/coverage/lsp-full.json"
jobs="${KRUSTY_TEST_JOBS:-1}"
# Default the per-binary thread count to the host's cores: the e2e binary IS the coverage workload
# (measured 244s of a ~11min CI job at the old fixed 3), and its tests mostly wait on pooled JVMs,
# so threads scale it near-linearly. The JVM pools are themselves host-scaled and heap-capped
# (see tests/common `server_pool_cap`), which bounds the memory pressure that once forced a low
# fixed value here. Override with KRUSTY_TEST_THREADS to pin it back down on a starved host.
test_threads="${KRUSTY_TEST_THREADS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu)}"
coverage_target="${KRUSTY_COVERAGE_TARGET_DIR:-target/coverage-build}"
test_timeout="${KRUSTY_COVERAGE_TEST_TIMEOUT_SECONDS:-120}"
e2e_timeout="${KRUSTY_COVERAGE_E2E_TIMEOUT_SECONDS:-300}"
source scripts/test-deadline.sh

# Self-provision the reference kotlinc + box corpus exactly like run-tests.sh, so the kept e2e
# suites (which need the stdlib jar / JVM runtime) don't silently skip and undercount coverage.
if command -v just >/dev/null 2>&1; then
  v="$(just max-version)"
  just kotlinc "$v" >/dev/null
  just box-corpus "$v" >/dev/null
fi

is_excluded() { local n="$1" e; for e in "${EXCLUDE[@]}"; do [ "$n" = "$e" ] && return 0; done; return 1; }

echo "coverage: instrumenting (nightly, branch), building test binaries…" >&2
if ! cargo +nightly llvm-cov --version >/dev/null 2>&1; then
  echo "coverage: cargo-llvm-cov is required; install with \`cargo install cargo-llvm-cov --locked\`" >&2
  echo "coverage: nightly llvm-tools are also required: \`rustup component add llvm-tools-preview --toolchain nightly\`" >&2
  exit 2
fi
# Keep instrumented coverage builds isolated from normal `target/debug` artifacts. `llvm-cov report`
# discovers coverage mappings from the instrumented binaries in the active target dir; reusing the
# normal target can accidentally include stale mappings from previous local runs or overlapping hooks.
rm -rf "$coverage_target"
export CARGO_TARGET_DIR="$coverage_target"
# Instrument the whole build (source-based coverage) for the rest of this script's cargo invocations.
source <(cargo +nightly llvm-cov show-env --sh --branch 2>/dev/null)
mkdir -p target/coverage
# Prune stale counters so this run measures only the tests it runs. `cargo llvm-cov clean` refuses a
# target/ it didn't create (missing CACHEDIR.TAG — e.g. a worktree whose target was set up by hand),
# so remove the raw/merged coverage files directly instead; profraw names carry a %p pid slot.
rm -f "$coverage_target"/*.profraw target/coverage/*.profdata target/coverage/*.profraw

# Compile the compiler and LSP coverage workloads without running them, then read each test
# executable's path from Cargo's JSON build output. The dedicated `coverage` profile (Cargo.toml)
# builds at opt-level 1 with gate-matching checks: the e2e suite runs krusty in-process for every
# dependency-lib fixture, and instrumenting that at `dev` made the coverage run dominate CI.
cargo +nightly build --profile coverage -p krusty-cli
export KRUSTY_BIN="$coverage_target/coverage/krusty"
if [ ! -x "$KRUSTY_BIN" ]; then
  echo "coverage: compiler binary missing after workspace build: $KRUSTY_BIN" >&2
  exit 1
fi
mapfile -t bins < <(
  {
    cargo +nightly test --no-run --profile coverage --lib --test e2e --message-format=json 2>/dev/null
    cargo +nightly test --no-run --profile coverage -p krusty-lsp --all-targets --message-format=json 2>/dev/null
  } | jq -r 'select(.profile.test == true and .executable != null) | .executable'
)

# Keep the lib/bin unit-test executables and every integration binary except the excluded suites.
run=()
for b in "${bins[@]}"; do
  name="$(basename "$b" | sed 's/-[0-9a-f]*$//')"
  is_excluded "$name" && continue
  run+=("$b")
done
echo "coverage: running ${#run[@]} test binaries in parallel (-P $jobs, --test-threads=$test_threads, timeout=${test_timeout}s, e2e-timeout=${e2e_timeout}s), conformance binary excluded" >&2

# Run the binaries in parallel; each writes its own profraw (LLVM_PROFILE_FILE has a %p pid slot).
# A non-zero exit from any binary (a failing test) fails the whole run — the tests are the workload.
# `--test-threads` is bounded explicitly instead of leaving libtest at nproc. The e2e target is one
# large binary, so forcing it to 1 serializes almost the entire coverage workload; using a small
# default preserves full coverage while avoiding the memory pressure seen with unbounded parallelism.
run_coverage_test_binary() {
  local binary="$1" status_root="$2" threads="$3" seconds="$4" e2e_seconds="$5"
  local name="$(basename "$binary")"
  local result="$status_root/$name"
  if [[ "$name" == e2e-* ]]; then
    seconds="$e2e_seconds"
  fi
  mkdir -p "$result"
  if run_with_deadline "$seconds" "$binary" --quiet --test-threads="$threads" \
      >"$result/output.log" 2>&1; then
    rm -rf "$result"
    return 0
  else
    local status="$?"
  fi
  printf '%s\n' "$status" >"$result/status"
  if [ "$status" -eq 124 ]; then
    printf 'coverage: TIMEOUT after %ss: %s --quiet --test-threads=%s\n' \
      "$seconds" "$binary" "$threads" >>"$result/output.log"
  fi
}
export -f run_coverage_test_binary

status_dir="$(mktemp -d)"
printf '%s\0' "${run[@]}" | xargs -0 -P "$jobs" -I{} \
  bash -c 'run_coverage_test_binary "$@"' _ {} "$status_dir" "$test_threads" "$test_timeout" "$e2e_timeout"
if compgen -G "$status_dir/*" >/dev/null; then
  echo "coverage: FAIL — test binaries reported failures:" >&2
  for result in "$status_dir"/*; do
    status="$(cat "$result/status")"
    echo "coverage: $(basename "$result") exited with status $status" >&2
    cat "$result/output.log" >&2
  done
  rm -rf "$status_dir"; exit 1
fi
rm -rf "$status_dir"

# Cargo package selection also controls which instrumented objects `llvm-cov report` discovers.
# Export each product package separately, then combine their totals for the repository gate.
IGNORE='(^|/)tests/|(^|/)src/main\.rs|(^|/)src/bin/'
cargo +nightly llvm-cov report --branch --profile coverage --ignore-filename-regex "$IGNORE" \
  --json --output-path "$compiler_raw_out"
cargo +nightly llvm-cov report --branch --profile coverage -p krusty-lsp --ignore-filename-regex "$IGNORE" \
  --json --output-path "$lsp_raw_out"

# Reduce both exports to the combined totals the gate compares against.
jq -s '
  map(.data[0].totals) as $totals
  | ["regions", "functions", "lines", "branches"]
  | map(. as $metric
      | ($totals | map(.[$metric].covered) | add) as $covered
      | ($totals | map(.[$metric].count) | add) as $count
      | {key: $metric, value: {
          covered: $covered,
          count: $count,
          percent: (if $count == 0 then 0 else $covered * 100 / $count end)
        }})
  | from_entries' \
  "$compiler_raw_out" "$lsp_raw_out" > "$summary_out"

echo "coverage summary ($summary_out):" >&2
jq -r 'to_entries[] | "  \(.key | (. + "         ")[0:10])  \(.value.percent*100|round/100)%  (\(.value.covered)/\(.value.count))"' "$summary_out" >&2
