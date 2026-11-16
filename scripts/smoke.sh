#!/usr/bin/env bash
# Pre-push smoke check: the failures most red tries are made of, in minutes instead of a full CI run.
# It is not the gate (`./run-tests.sh` and CI are); it is what to run before every push.
#
#   scripts/smoke.sh [base-rev]        base defaults to origin/master; use the parent PR's branch
#                                      (`origin/<parent-branch>`) for a stacked PR
#
# In order, stopping at the first failure:
#   1. attribution and outcome lists of the commits in base..HEAD;
#   2. rustfmt, then the clippy and legacy baselines;
#   3. the lib tests of krusty and the backend crates;
#   4. the e2e modules (`tests/*_e2e.rs`) this branch changes;
#   5. a box slice on JVM, Native, wasm-js and wasm-wasi: an even sample of the corpus plus every
#      owned regression (`tests/box_regressions/`) this branch adds or changes.
#
# Knobs: KRUSTY_SMOKE_BOX_LIMIT (corpus cases sampled per lane, default 150; 0 runs the changed
# regressions only), KRUSTY_SMOKE_LANES (default "jvm native wasm-js wasm-wasi"),
# KRUSTY_SMOKE_SKIP_CLIPPY=1 (skip step 2's clippy run, which is the slowest static check).
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
cd "$repo_root"

base="${1:-origin/master}"
git rev-parse --verify --quiet "$base^{commit}" >/dev/null || {
  echo "smoke: unknown base revision: $base" >&2
  exit 2
}
fork="$(git merge-base "$base" HEAD)"
limit="${KRUSTY_SMOKE_BOX_LIMIT:-150}"
lanes="${KRUSTY_SMOKE_LANES:-jvm native wasm-js wasm-wasi}"
start=$SECONDS

step() { printf '\nsmoke: [%3ss] %s\n' "$((SECONDS - start))" "$*" >&2; }

changed() { git diff --name-only --diff-filter=AMR "$fork" HEAD -- "$@"; }

step "attribution and outcome lists ($fork..HEAD)"
bash scripts/check-attribution.sh "$fork..HEAD"
bash scripts/check-outcome-lists.sh "$fork" HEAD

step "rustfmt"
cargo fmt --all -- --check
step "legacy baseline"
just legacy-baseline-check
if [ "${KRUSTY_SMOKE_SKIP_CLIPPY:-}" != 1 ]; then
  step "clippy baseline"
  just clippy-baseline-check
fi

# The lanes and the target lib tests read the exact reference toolchain of the version under test.
v="${KRUSTY_LANGUAGE_VERSION:-$(just max-version)}"
export KRUSTY_LANGUAGE_VERSION="$v"
export KRUSTY_KOTLINC="${KRUSTY_KOTLINC:-$(just kotlinc "$v")}"
export KRUSTY_KOTLIN_BOX_DIR="${KRUSTY_KOTLIN_BOX_DIR:-$(just box-corpus "$v")}"
export KRUSTY_KOTLIN_NATIVE="${KRUSTY_KOTLIN_NATIVE:-$(just kotlin-native "$v")}"
export KRUSTY_DEPS_CACHE="${KRUSTY_DEPS_CACHE:-$PWD/target/cache/krusty-deps}"
export RUST_MIN_STACK="${RUST_MIN_STACK:-134217728}"

step "lib tests"
cargo test -q --profile gate -p krusty -p krusty-native -p krusty-wasm -p krusty-jvm --lib

e2e_modules=()
while IFS= read -r file; do
  e2e_modules+=("$(basename "$file" .rs)::")
done < <(changed 'tests/*_e2e.rs')
if [ "${#e2e_modules[@]}" -gt 0 ]; then
  step "changed e2e modules: ${e2e_modules[*]}"
  e2e_bin="$(scripts/cargo-test-binary.sh e2e --test e2e)"
  "$e2e_bin" "${e2e_modules[@]}"
fi

regressions=()
while IFS= read -r file; do
  regressions+=("regressions/${file#tests/box_regressions/}")
done < <(changed 'tests/box_regressions/*.kt' 'tests/box_regressions/**/*.kt')

step "building the conformance binary"
bin="$(scripts/cargo-test-binary.sh conformance --test conformance)"

# One lane: the sampled corpus slice, then each changed regression by its path. A narrowed run
# checks the expected-failure lists over exactly the cases it scanned.
run_lane() {
  local lane=$1 infix test only
  case "$lane" in
    jvm) infix="" test=kotlin_box_ir_jvm_conformance::kotlin_codegen_box_conformance ;;
    native) infix=NATIVE_ test=kotlin_box_native_conformance::kotlin_codegen_box_native_conformance ;;
    wasm-js) infix=WASM_JS_ test=kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_js_conformance ;;
    wasm-wasi) infix=WASM_WASI_ test=kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_wasi_conformance ;;
    *) echo "smoke: unknown lane: $lane" >&2; return 2 ;;
  esac
  local require=()
  [ -z "$infix" ] || require=("KRUSTY_REQUIRE_${infix}CONFORMANCE=1")
  if [ "$limit" -gt 0 ]; then
    step "$lane: $limit sampled corpus cases"
    env "${require[@]}" "KRUSTY_${infix}BOX_LIMIT=$limit" "$bin" --exact "$test" --nocapture
  fi
  for only in "${regressions[@]}"; do
    step "$lane: regression $only"
    env "${require[@]}" "KRUSTY_${infix}BOX_ONLY=$only" "$bin" --exact "$test" --nocapture
  done
}

for lane in $lanes; do
  run_lane "$lane"
done

step "passed"
