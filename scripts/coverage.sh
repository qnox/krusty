#!/usr/bin/env bash
# Measure test coverage (regions, functions, lines, branches) via LLVM source-based coverage.
#
# Branch coverage needs the nightly `-Zcoverage-options=branch` path, so the whole run goes through
# one pinned nightly toolchain. Coverage mappings and therefore the committed baseline can change
# between compiler builds even when repository code does not.
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

# The coverage build is instrumented at opt-level 1 (`[profile.coverage]`), so its stack frames are
# still larger than the gate build's. The deepest compiler paths — e.g.
# `inline_deep_coverage_e2e::inline_five_levels_deep`, which recurses through the inline-function
# splicer five levels down — can sit near libtest's default per-test thread stack (~2 MiB) and
# overflow it non-deterministically: an unrelated code change that shifts frame layout is enough to
# tip a passing run into a `stack overflow, aborting` (which fails the whole binary → the gate).
# Give the test threads a generous stack so legitimate deep recursion never aborts.
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}" # 128 MiB

summary_out="${1:-target/coverage/summary.json}"
compiler_raw_out="${KRUSTY_COVERAGE_COMPILER_JSON:-target/coverage/compiler-full.json}"
lsp_raw_out="${KRUSTY_COVERAGE_LSP_JSON:-target/coverage/lsp-full.json}"
jobs="${KRUSTY_TEST_JOBS:-1}"
# Default the per-binary thread count to the host's cores: the e2e binary IS the coverage workload
# (measured 244s of a ~11min CI job at the old fixed 3), and its tests mostly wait on pooled JVMs,
# so threads scale it near-linearly. The JVM pools are themselves host-scaled and heap-capped
# (see tests/common `server_pool_cap`), which bounds the memory pressure that once forced a low
# fixed value here. Override with KRUSTY_TEST_THREADS to pin it back down on a starved host.
test_threads="${KRUSTY_TEST_THREADS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu)}"
coverage_target="${KRUSTY_COVERAGE_TARGET_DIR:-target/coverage-build}"
coverage_toolchain="${KRUSTY_COVERAGE_TOOLCHAIN:-nightly-2026-09-05}"
coverage_cargo=(cargo "+${coverage_toolchain}")
test_timeout="${KRUSTY_COVERAGE_TEST_TIMEOUT_SECONDS:-120}"
# The e2e suite is one process. An instrumented run is about seventeen minutes; thirty minutes
# leaves room for a slow runner and still fails a hung binary.
e2e_timeout="${KRUSTY_COVERAGE_E2E_TIMEOUT_SECONDS:-1800}"
source scripts/test-deadline.sh
source scripts/libtest-shards.sh
source scripts/phase-timing.sh
coverage_slow_test_threshold="$(libtest_slow_test_threshold "${KRUSTY_SLOW_TEST_MS:-200}")"
export KRUSTY_COVERAGE_SLOW_TEST_THRESHOLD="$coverage_slow_test_threshold"
phase_log_prefix=coverage
export phase_log_prefix
PHASE_TIMING_LOG="$(mktemp)"
export PHASE_TIMING_LOG
export -f phase_begin phase_end
trap 'phase_report || true' EXIT

# Self-provision the reference kotlinc + box corpus exactly like run-tests.sh, so the kept e2e
# suites (which need the stdlib jar / JVM runtime) don't silently skip and undercount coverage.
phase_begin provision
if command -v just >/dev/null 2>&1; then
  v="$(just max-version)"
  just kotlinc "$v" >/dev/null
  just box-corpus "$v" >/dev/null
fi
phase_end provision

is_excluded() { local n="$1" e; for e in "${EXCLUDE[@]}"; do [ "$n" = "$e" ] && return 0; done; return 1; }

echo "coverage: instrumenting (${coverage_toolchain}, branch), building test binaries…" >&2
if ! "${coverage_cargo[@]}" llvm-cov --version >/dev/null 2>&1; then
  echo "coverage: cargo-llvm-cov is required; install with \`cargo install cargo-llvm-cov --locked\`" >&2
  echo "coverage: pinned nightly llvm-tools are also required: \`rustup component add llvm-tools-preview --toolchain ${coverage_toolchain}\`" >&2
  exit 2
fi
phase_begin instrument
# Keep instrumented coverage builds isolated from normal `target/debug` artifacts. `llvm-cov report`
# discovers coverage mappings from the instrumented binaries in the active target dir; reusing the
# normal target can accidentally include stale mappings from previous local runs or overlapping hooks.
rm -rf "$coverage_target"
export CARGO_TARGET_DIR="$coverage_target"
# Instrument the whole build (source-based coverage) for the rest of this script's cargo invocations.
source <("${coverage_cargo[@]}" llvm-cov show-env --sh --branch 2>/dev/null)
# The frontend of one rustc is otherwise single-threaded. Asking for one frontend job per core
# sped the instrumented CLI and language-server build; the same `-C instrument-coverage` and
# `-Z coverage-options=branch` flags are still what the wrapper passes through.
frontend_threads="$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 1)"
case "$frontend_threads" in
  ''|*[!0-9]*) frontend_threads=1 ;;
esac
if [ "$frontend_threads" -gt 1 ]; then
  if [ -n "${RUSTFLAGS:-}" ]; then
    export RUSTFLAGS="$RUSTFLAGS -Z threads=${frontend_threads}"
  else
    export RUSTFLAGS="-Z threads=${frontend_threads}"
  fi
fi
mkdir -p target/coverage
# Prune stale counters so this run measures only the tests it runs. `cargo llvm-cov clean` refuses a
# target/ it didn't create (missing CACHEDIR.TAG — e.g. a worktree whose target was set up by hand),
# so remove the raw/merged coverage files directly instead; profraw names carry a %p pid slot.
rm -f "$coverage_target"/*.profraw target/coverage/*.profdata target/coverage/*.profraw
phase_end instrument

# Compile the product binaries and the compiler and language-server tests in one cargo build.
# A separate `cargo test` after `cargo build` compiles krusty again and cannot overlap that
# cfg(test) harness with the binaries. One invocation compiles the non-test library once, overlaps
# the cfg(test) harness with the binaries, and leaves the instrumentation flags unchanged.
# The dedicated `coverage` profile builds at opt-level 1: instrumenting the in-process e2e
# compiler at `dev` made that suite dominate CI.
run_phase() {
  local name="$1"
  shift
  phase_begin "$name"
  if "$@"; then
    phase_end "$name"
  else
    local status=$?
    phase_end "$name"
    exit "$status"
  fi
}
# Cargo's human progress stays on stderr while its machine-readable artifact stream goes to the
# temporary file. The executable query finds the test binaries without a second Cargo invocation.
read_test_executables() {
  jq -r '
    def package_is($name):
      test("(^|[/# ])" + $name + "([#@ ]|$)");
    select(.reason == "compiler-artifact" and .profile.test == true and .executable != null)
    | . as $artifact
    | ($artifact.package_id | tostring) as $package
    | ($artifact.target.kind | join(",")) as $kind
    | if ($package | package_is("krusty-lsp")) then
        $artifact.executable
      elif ($package | package_is("krusty-cli")) and $kind == "lib" then
        $artifact.executable
      elif ($package | package_is("krusty"))
          and ($kind == "lib" or ($kind == "test" and $artifact.target.name == "e2e")) then
        $artifact.executable
      else
        empty
      end
  '
}
phase_begin build-compiler
compiler_json="$(mktemp)"
if "${coverage_cargo[@]}" build --profile coverage --message-format=json \
    -p krusty -p krusty-cli -p krusty-lsp --lib --bins --tests >"$compiler_json"; then
  phase_end build-compiler
else
  status="$?"
  # JSON mode keeps Cargo progress on stderr, but rustc diagnostics are in the artifact stream.
  # Replay rendered failures before deleting it so a broken coverage build remains diagnosable.
  jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' \
    "$compiler_json" >&2 || true
  phase_end build-compiler
  rm -f "$compiler_json"
  exit "$status"
fi
export KRUSTY_BIN="$coverage_target/coverage/krusty"
# Bin unit tests exec the supervisor. `cargo test --bin` builds the harness only, so publish the
# uplifted binary the same way integration tests do.
export KRUSTY_LSP_BIN="$coverage_target/coverage/krusty-lsp"
if [ ! -x "$KRUSTY_BIN" ]; then
  echo "coverage: compiler binary missing after workspace build: $KRUSTY_BIN" >&2
  exit 1
fi
if [ ! -x "$KRUSTY_LSP_BIN" ]; then
  echo "coverage: language server binary missing after workspace build: $KRUSTY_LSP_BIN" >&2
  exit 1
fi
mapfile -t bins < <(read_test_executables <"$compiler_json")
rm -f "$compiler_json"

# Keep the lib/bin unit-test executables and every integration binary except the excluded suites.
run=()
e2e_bin=""
for b in "${bins[@]}"; do
  name="$(basename "$b" | sed 's/-[0-9a-f]*$//')"
  is_excluded "$name" && continue
  if [ "$name" = e2e ]; then
    e2e_bin="$b"
    continue
  fi
  run+=("$b")
done
echo "coverage: running ${#run[@]} test binaries in parallel (-P $jobs, --test-threads=$test_threads, timeout=${test_timeout}s, e2e-timeout=${e2e_timeout}s), conformance binary excluded" >&2

# Run the binaries in parallel; each writes its own profraw (LLVM_PROFILE_FILE has a %p pid slot).
# A non-zero exit from any binary (a failing test) fails the whole run — the tests are the workload.
# `--test-threads` is bounded explicitly instead of leaving libtest at nproc. The e2e target is one
# large binary, so forcing it to 1 serializes almost the entire coverage workload; using a small
# default preserves full coverage while avoiding the memory pressure seen with unbounded parallelism.
# `--quiet` hides the per-test duration, so coverage keeps the listing and records tests over the
# slow threshold before a passing log is deleted.
record_coverage_slow_tests() {
  local log="$1" label="$2" storage_key="${3:-$2}"
  [ -n "${KRUSTY_SLOW_TEST_DIR:-}" ] && [ -f "$log" ] || return 0
  libtest_slow_tests "$log" "$KRUSTY_COVERAGE_SLOW_TEST_THRESHOLD" | while IFS=$'\t' read -r ms test_name; do
    printf '%s\t%s\t%s\n' "$ms" "$label" "$test_name"
  done >"$KRUSTY_SLOW_TEST_DIR/$storage_key.tsv"
}
export -f record_coverage_slow_tests

run_coverage_test_binary() {
  local binary="$1" status_root="$2" threads="$3" seconds="$4"
  local raw name phase result status
  raw="$(basename "$binary")"
  name="$(printf '%s\n' "$raw" | sed 's/-[0-9a-f]*$//')"
  phase="test-$name"
  result="$status_root/$raw"
  mkdir -p "$result"
  status=0
  phase_begin "$phase"
  if run_with_deadline "$seconds" "$binary" -Z unstable-options --report-time --color never \
      --test-threads="$threads" >"$result/output.log" 2>&1; then
    :
  else
    status="$?"
  fi
  phase_end "$phase"
  # Two Cargo targets can have the same hash-stripped name (`krusty_lsp`). Keep their readable
  # labels, but write through the unique executable basename so concurrent records never overwrite.
  record_coverage_slow_tests "$result/output.log" "$name" "$raw"
  if [ "$status" -eq 0 ]; then
    rm -rf "$result"
    return 0
  fi
  printf '%s\n' "$status" >"$result/status"
  if [ "$status" -eq 124 ]; then
    printf 'coverage: TIMEOUT after %ss: %s --report-time --test-threads=%s\n' \
      "$seconds" "$binary" "$threads" >>"$result/output.log"
  fi
}
export -f run_coverage_test_binary

status_dir="$(mktemp -d)"
slow_dir="$(mktemp -d)"
export KRUSTY_SLOW_TEST_DIR="$slow_dir"
printf '%s\0' "${run[@]}" | xargs -0 -P "$jobs" -I{} \
  bash -c 'run_coverage_test_binary "$@"' _ {} "$status_dir" "$test_threads" "$test_timeout"

# The e2e binary is internally parallel and owns the cores, so it runs alone after the small binaries.
if [ -n "$e2e_bin" ]; then
  echo "coverage: e2e (timeout=${e2e_timeout}s)" >&2
  run_coverage_test_binary "$e2e_bin" "$status_dir" "$test_threads" "$e2e_timeout"
fi
slow_combined="$(mktemp)"
if compgen -G "$slow_dir/*.tsv" >/dev/null; then
  cat "$slow_dir"/*.tsv >"$slow_combined"
fi
libtest_print_slow_records "$coverage_slow_test_threshold" "$slow_combined"
rm -f "$slow_combined"
rm -rf "$slow_dir"
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
run_phase report-compiler \
  "${coverage_cargo[@]}" llvm-cov report --branch --profile coverage --ignore-filename-regex "$IGNORE" \
  --json --output-path "$compiler_raw_out"
run_phase report-lsp \
  "${coverage_cargo[@]}" llvm-cov report --branch --profile coverage -p krusty-lsp --ignore-filename-regex "$IGNORE" \
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
