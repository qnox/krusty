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

summary_out="${1:-target/coverage/summary.json}"
raw_out="target/coverage/full.json"
jobs="${KRUSTY_TEST_JOBS:-1}"
coverage_target="${KRUSTY_COVERAGE_TARGET_DIR:-target/coverage-build}"

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

# Compile exactly the coverage workload without running it, and read each test executable's path from
# cargo's JSON build output. Keep library unit tests plus the product e2e integration target; do not
# instrument/build zero-test bin harnesses or `conformance`, because conformance is excluded from the
# metric and pre-push runs it separately without coverage instrumentation.
mapfile -t bins < <(cargo +nightly test --no-run --lib --test e2e --message-format=json 2>/dev/null \
  | jq -r 'select(.profile.test == true and .executable != null) | .executable')

# Keep the lib/bin unit-test executables and every integration binary except the excluded suites.
run=()
for b in "${bins[@]}"; do
  name="$(basename "$b" | sed 's/-[0-9a-f]*$//')"
  is_excluded "$name" && continue
  run+=("$b")
done
echo "coverage: running ${#run[@]} test binaries in parallel (-P $jobs), conformance binary excluded" >&2

# Run the binaries in parallel; each writes its own profraw (LLVM_PROFILE_FILE has a %p pid slot).
# A non-zero exit from any binary (a failing test) fails the whole run — the tests are the workload.
# `--test-threads=1` per binary is deliberate: -P already gives jobs-wide across-binary parallelism,
# so one thread each keeps total concurrency at `jobs`; letting each binary default to nproc threads
# would make it jobs×nproc-wide and thrash the cores (much slower under coverage instrumentation).
status_dir="$(mktemp -d)"
printf '%s\0' "${run[@]}" | xargs -0 -P "$jobs" -I{} \
  sh -c '"$1" --quiet --test-threads=1 2>/dev/null || echo fail > "$2/$(basename "$1")"' _ {} "$status_dir"
if compgen -G "$status_dir/*" >/dev/null; then
  echo "coverage: FAIL — test binaries reported failures:" >&2
  ls "$status_dir" >&2
  rm -rf "$status_dir"; exit 1
fi
rm -rf "$status_dir"

# Coverage is of the product (src/ library), not of the test harness or the CLI/survey tooling.
IGNORE='(^|/)tests/|(^|/)src/main\.rs|(^|/)src/bin/'
cargo +nightly llvm-cov report --branch --ignore-filename-regex "$IGNORE" \
  --json --output-path "$raw_out"

# Reduce llvm-cov's export to the four totals the gate compares against. `percent` is already 0..100.
jq '.data[0].totals
    | { regions:   {covered: .regions.covered,   count: .regions.count,   percent: .regions.percent},
        functions: {covered: .functions.covered, count: .functions.count, percent: .functions.percent},
        lines:     {covered: .lines.covered,     count: .lines.count,     percent: .lines.percent},
        branches:  {covered: .branches.covered,  count: .branches.count,  percent: .branches.percent} }' \
  "$raw_out" > "$summary_out"

echo "coverage summary ($summary_out):" >&2
jq -r 'to_entries[] | "  \(.key | (. + "         ")[0:10])  \(.value.percent*100|round/100)%  (\(.value.covered)/\(.value.count))"' "$summary_out" >&2
