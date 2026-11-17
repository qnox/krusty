#!/usr/bin/env bash
# Pre-push smoke check: the failures most red tries are made of, in minutes instead of a full CI run.
# It is not the gate (`./run-tests.sh` and CI are); it is what to run before every push.
#
#   scripts/smoke.sh [base-rev]        base defaults to origin/master; use the parent PR's branch
#                                      (`origin/<parent-branch>`) for a stacked PR
#
# In order, stopping at the first failure:
#   1. attribution and outcome lists of the commits in base..HEAD;
#   2. rustfmt as CI checks it (`just fmt-check`, which also covers `tests/*_e2e.rs`), then the
#      legacy and clippy baselines;
#   3. the lib tests of krusty and the backend crates;
#   4. the e2e modules (`tests/*_e2e.rs`) this branch changes;
#   5. a box slice on JVM, Native, wasm-js and wasm-wasi: an even sample of the corpus, then the
#      owned regressions (`tests/box_regressions/`): all of them on JVM and Wasm, the ones this
#      branch changes on Native (see run_lane).
#
# Every test process runs under the harness's configurable deadlines (scripts/test-gate-defaults.sh).
#
# Knobs: KRUSTY_SMOKE_BOX_LIMIT (corpus cases sampled per lane, default 150; 0 runs the owned
# regressions only), KRUSTY_SMOKE_LANES (default "jvm native wasm-js wasm-wasi"),
# KRUSTY_SMOKE_SKIP_CLIPPY=1 (skip step 2's clippy run, which is the slowest static check).
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
cd "$repo_root"
source scripts/test-gate-defaults.sh
source scripts/test-deadline.sh

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
just fmt-check
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
# A local checkout has no published recorded-byte archive, so the reference compiler compiles a
# missing class dump instead of refusing it, as the conformance-regressions run does.
export KRUSTY_CLASS_DUMP_COMPILE_MISSING="${KRUSTY_CLASS_DUMP_COMPILE_MISSING:-1}"

step "lib tests"
for package in krusty krusty-native krusty-wasm krusty-jvm; do
  lib_bin="$(scripts/cargo-test-binary.sh "${package//-/_}" -p "$package" --lib)"
  run_with_deadline "$KRUSTY_TEST_TIMEOUT_SECONDS" "$lib_bin" -q
done

e2e_modules=()
while IFS= read -r file; do
  e2e_modules+=("$(basename "$file" .rs)::")
done < <(changed 'tests/*_e2e.rs')
if [ "${#e2e_modules[@]}" -gt 0 ]; then
  step "changed e2e modules: ${e2e_modules[*]}"
  e2e_bin="$(scripts/cargo-test-binary.sh e2e --test e2e)"
  run_with_deadline "$KRUSTY_E2E_TIMEOUT_SECONDS" "$e2e_bin" "${e2e_modules[@]}"
fi

# The owned regressions this branch adds or changes, by their path under the repository.
regressions=()
while IFS= read -r file; do
  regressions+=("/$file")
done < <(changed 'tests/box_regressions/*.kt' 'tests/box_regressions/**/*.kt')

step "building the conformance binary"
bin="$(scripts/cargo-test-binary.sh conformance --test conformance)"

# One lane: the sampled corpus slice, then owned regressions. JVM, wasm-js and wasm-wasi run every
# owned regression; their inventory keeps only the ones that apply to the release under test.
# Native runs only the regressions this branch changes, because each multi-module one first runs
# kotlinc-native as its oracle, which takes minutes for the whole set; a changed regression outside
# the release under test selects nothing there. A narrowed run checks the expected-failure lists over
# exactly the cases it scanned.
run_lane() {
  local lane=$1 infix test deadline only
  case "$lane" in
    jvm) infix="" test=kotlin_box_ir_jvm_conformance::kotlin_codegen_box_conformance deadline=$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS ;;
    native) infix=NATIVE_ test=kotlin_box_native_conformance::kotlin_codegen_box_native_conformance deadline=$KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS ;;
    wasm-js) infix=WASM_JS_ test=kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_js_conformance deadline=$KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS ;;
    wasm-wasi) infix=WASM_WASI_ test=kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_wasi_conformance deadline=$KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS ;;
    *) echo "smoke: unknown lane: $lane" >&2; return 2 ;;
  esac
  local require=()
  [ -z "$infix" ] || require=("KRUSTY_REQUIRE_${infix}CONFORMANCE=1")
  if [ "$limit" -gt 0 ]; then
    step "$lane: $limit sampled corpus cases"
    run_with_deadline "$deadline" env "${require[@]}" "KRUSTY_${infix}BOX_LIMIT=$limit" \
      "$bin" --exact "$test" --nocapture
  fi
  if [ "$lane" != native ]; then
    step "$lane: owned regressions"
    run_with_deadline "$deadline" env "${require[@]}" "KRUSTY_${infix}BOX_ONLY=/tests/box_regressions/" \
      "$bin" --exact "$test" --nocapture
    return
  fi
  for only in "${regressions[@]}"; do
    step "$lane: changed regression $only"
    run_with_deadline "$deadline" env "${require[@]}" "KRUSTY_${infix}BOX_ONLY=$only" \
      "$bin" --exact "$test" --nocapture
  done
}

for lane in $lanes; do
  run_lane "$lane"
done

step "passed"
