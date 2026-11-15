#!/usr/bin/env bash
# Run krusty's own test suite, uninstrumented, in the `gate` profile: CI's merge gate.
#
# The binaries are the ones instrumented coverage measures (scripts/own-test-binaries.jq) except the
# product `e2e` target, which CI builds in build-tool-bins and runs in its own job. Coverage runs
# nightly (.github/workflows/coverage.yml). The compiler's unit binary is published as
# `compiler_unit_tests` in GITHUB_OUTPUT for the KLIB implementation ratchet, which runs it again.
set -euo pipefail
declare +x own_tests_github_output="${GITHUB_OUTPUT:-}"
unset GITHUB_OUTPUT
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."

# Deep compiler recursion needs more than libtest's default thread stack; see run-tests.sh.
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}" # 128 MiB
source scripts/test-gate-defaults.sh
source scripts/test-deadline.sh
source scripts/phase-timing.sh
phase_log_prefix=own-tests
PHASE_TIMING_LOG="$(mktemp)"
export PHASE_TIMING_LOG
trap 'phase_report || true' EXIT

test_threads="${KRUSTY_TEST_THREADS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu)}"
target_root="${CARGO_TARGET_DIR:-$PWD/target}"

phase_begin provision
if command -v just >/dev/null 2>&1; then
  v="$(just max-version)"
  just kotlinc "$v" >/dev/null
  just box-corpus "$v" >/dev/null
fi
phase_end provision

# Build one selection and print its own-suite test executables. Rendered rustc diagnostics are only
# in Cargo's JSON stream, so they are mirrored to stderr.
build_selection() {
  local stream
  stream="$(mktemp)"
  if ! cargo "$@" --profile gate --message-format=json >"$stream"; then
    jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' "$stream" >&2 || true
    rm -f "$stream"
    return 1
  fi
  jq -r -f scripts/own-test-binaries.jq "$stream"
  rm -f "$stream"
}

# The backend, CLI, language server and toolchain crates with the CLI and language-server binaries
# their tests start, then the compiler's cfg(test) unit harness. Cargo applies `--tests` to every
# selected package, so the root package is built apart: with it, `--tests` would also build the
# conformance and e2e targets this job does not run.
phase_begin build
bins=()
mapfile -t -O "${#bins[@]}" bins < <(build_selection build \
  -p krusty-jvm -p krusty-native -p krusty-wasm -p krusty-cli -p krusty-lsp -p krusty-toolchain \
  --lib --bins --tests)
compiler_unit_tests="$(build_selection test --no-run -p krusty --lib)"
phase_end build
[ -n "$compiler_unit_tests" ] || { echo "own-tests: compiler unit binary missing" >&2; exit 1; }
bins+=("$compiler_unit_tests")

export KRUSTY_BIN="$target_root/gate/krusty"
export KRUSTY_LSP_BIN="$target_root/gate/krusty-lsp"
for binary in "$KRUSTY_BIN" "$KRUSTY_LSP_BIN"; do
  [ -x "$binary" ] || { echo "own-tests: binary missing after the build: $binary" >&2; exit 1; }
done

failed=()
for binary in "${bins[@]}"; do
  name="$(basename "$binary" | sed 's/-[0-9a-f]*$//')"
  log="$(mktemp)"
  phase_begin "test-$name"
  status=0
  run_with_deadline "$KRUSTY_TEST_TIMEOUT_SECONDS" "$binary" --test-threads="$test_threads" \
    >"$log" 2>&1 || status=$?
  phase_end "test-$name"
  if [ "$status" -ne 0 ]; then
    [ "$status" -ne 124 ] || echo "own-tests: TIMEOUT after ${KRUSTY_TEST_TIMEOUT_SECONDS}s" >>"$log"
    echo "own-tests: $binary exited with status $status" >&2
    cat "$log" >&2
    failed+=("$name")
  fi
  rm -f "$log"
done
if [ "${#failed[@]}" -ne 0 ]; then
  echo "own-tests: FAIL — ${failed[*]}" >&2
  exit 1
fi

if [ -n "$own_tests_github_output" ]; then
  printf 'compiler_unit_tests=%s\n' "$compiler_unit_tests" >>"$own_tests_github_output"
fi
