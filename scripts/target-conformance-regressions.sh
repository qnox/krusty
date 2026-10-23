#!/usr/bin/env bash
# Run one runnable target's focused tests after its exact toolchain and KLIBs are provisioned.
# The box inventory, including owned module graphs, stays in its dedicated corpus process.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <native|wasm> <conformance-bin> <kotlin-version>" >&2
  exit 2
fi

target="$1"
bin="$2"
v="$3"
[ -x "$bin" ] || { echo "conformance binary is not executable: $bin" >&2; exit 1; }

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$script_dir/test-gate-defaults.sh"
source "$script_dir/test-deadline.sh"

case "$target" in
  native)
    : "${KRUSTY_KOTLIN_NATIVE:?native target regressions require KRUSTY_KOTLIN_NATIVE}"
    export KRUSTY_REQUIRE_NATIVE_CONFORMANCE=1
    filter=kotlin_box_native_conformance::
    skips=(--skip kotlin_box_native_conformance::kotlin_codegen_box_native_conformance)
    ;;
  wasm)
    : "${KRUSTY_KOTLINC:?wasm target regressions require KRUSTY_KOTLINC}"
    : "${KRUSTY_DEPS_CACHE:?wasm target regressions require KRUSTY_DEPS_CACHE}"
    command -v node >/dev/null || { echo "wasm target regressions require node" >&2; exit 1; }
    export KRUSTY_REQUIRE_WASM_JS_CONFORMANCE=1
    export KRUSTY_REQUIRE_WASM_WASI_CONFORMANCE=1
    filter=kotlin_box_wasm_conformance::
    skips=(
      --skip kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_js_conformance
      --skip kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_wasi_conformance
    )
    ;;
  *)
    echo "unknown target regression lane: $target" >&2
    exit 2
    ;;
esac

export KRUSTY_LANGUAGE_VERSION="$v"
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"
set +e
run_with_deadline "$KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS" \
  "$bin" "$filter" "${skips[@]}" --test-threads 1
focused_status=$?
set -e
if [ "$focused_status" -eq 124 ]; then
  echo "target-conformance-regressions: timed out after ${KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS}s: Kotlin $v $target" >&2
fi
exit "$focused_status"
