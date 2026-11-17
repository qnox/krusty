#!/usr/bin/env bash
# Pre-push smoke check: the failures most red tries are made of, in minutes instead of a full CI run.
# It is not the gate (`./run-tests.sh` and CI are); it is what to run before every push.
#
#   scripts/smoke.sh [base-rev]        base defaults to origin/master; use the parent PR's branch
#                                      (`origin/<parent-branch>`) for a stacked PR, or the
#                                      branch's own remote head to check only unpushed commits
#
# The branch's own change is what its first-parent, non-merge commits in base..HEAD touch. Merges of
# master or of a parent pull request bring code their own runs already checked, so they select
# nothing here.
#
# In order, stopping at the first failure:
#   1. attribution and outcome lists of the commits in base..HEAD;
#   2. rustfmt as CI checks it (`just fmt-check`, which also covers `tests/*_e2e.rs`), the legacy
#      baseline, then the clippy baseline when the change touches Rust;
#   3. ONE cargo build of every test binary the later steps run, so the compiler is built once;
#   4. the lib tests of krusty and of the backend crates;
#   5. the e2e modules (`tests/*_e2e.rs`) this change touches;
#   6. a box slice on the lanes this change can affect (see select_lanes): an even sample of the
#      corpus, then the owned regressions (`tests/box_regressions/`): all of them on JVM and Wasm,
#      the ones this change touches on Native (see run_lane).
#
# Every test process runs under the harness's configurable deadlines (scripts/test-gate-defaults.sh).
#
# Knobs: KRUSTY_SMOKE_BOX_LIMIT (corpus cases sampled per lane, default 150; 0 runs the owned
# regressions only), KRUSTY_SMOKE_LANES (overrides the selected lanes, e.g. "jvm native wasm-js
# wasm-wasi"; empty runs none), KRUSTY_SMOKE_SKIP_CLIPPY=1 (skip the clippy run, the slowest static
# check: it lints the whole workspace from a clean target of its own).
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
start=$SECONDS

step() { printf '\nsmoke: [%3ss] %s\n' "$((SECONDS - start))" "$*" >&2; }

# Paths the branch's own commits add, modify or rename to, that still exist at HEAD.
changed() {
  git log --first-parent --no-merges --diff-filter=AMR --name-only --format= "$fork..HEAD" -- "$@" \
    | sort -u | while IFS= read -r file; do [ -e "$file" ] && printf '%s\n' "$file"; done
}

# The box lanes a change can affect. A backend crate, or its expected-failure lists, selects its own
# lanes; the shared KLIB lowering selects the KLIB lanes; the class-file layer selects JVM. Any other
# compiler source, a shared regression, the harness, or the build selects every lane. Scripts and
# documentation select none.
select_lanes() {
  local file jvm=0 native=0 wasm=0
  while IFS= read -r file; do
    case "$file" in
      crates/krusty-jvm/* | src/jvm/* | tests/box_expected_failures/jvm*) jvm=1 ;;
      crates/krusty-native/* | tests/box_expected_failures/native* | tests/klib_uncovered_implementations/native/*) native=1 ;;
      crates/krusty-wasm/* | tests/box_expected_failures/wasm*) wasm=1 ;;
      src/klib_lowering/* | src/klib/*) native=1 wasm=1 ;;
      *.md | docs/* | scripts/smoke.sh | .github/*) ;;
      src/* | crates/* | tests/* | build.rs | Cargo.toml | Cargo.lock | .cargo/* | justfile)
        jvm=1 native=1 wasm=1 ;;
    esac
  done < <(changed)
  local selected=()
  [ "$jvm" = 0 ] || selected+=(jvm)
  [ "$native" = 0 ] || selected+=(native)
  [ "$wasm" = 0 ] || selected+=(wasm-js wasm-wasi)
  printf '%s\n' "${selected[*]}"
}
lanes="${KRUSTY_SMOKE_LANES-$(select_lanes)}"

step "attribution and outcome lists ($fork..HEAD)"
bash scripts/check-attribution.sh "$fork..HEAD"
bash scripts/check-outcome-lists.sh "$fork" HEAD

step "rustfmt"
just fmt-check
step "legacy baseline"
just legacy-baseline-check
if [ "${KRUSTY_SMOKE_SKIP_CLIPPY:-}" != 1 ] && [ -n "$(changed '*.rs')" ]; then
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

e2e_modules=()
while IFS= read -r file; do
  e2e_modules+=("$(basename "$file" .rs)::")
done < <(changed 'tests/*_e2e.rs')

# The owned regressions this change adds or modifies, by their path under the repository.
regressions=()
while IFS= read -r file; do
  regressions+=("/$file")
done < <(changed 'tests/box_regressions/*.kt' 'tests/box_regressions/**/*.kt')

# One cargo invocation builds every binary the remaining steps run. Separate invocations per
# package rebuild the krusty library under each package's feature set; one invocation unifies them.
libs=(krusty krusty-native krusty-wasm krusty-jvm)
build=(--lib)
for package in "${libs[@]}"; do build+=(-p "$package"); done
[ -z "$lanes" ] || build+=(--test conformance)
[ "${#e2e_modules[@]}" = 0 ] || build+=(--test e2e)
step "building test binaries (${build[*]})"
artifacts="$(mktemp)"
trap 'rm -f "$artifacts"' EXIT
cargo test --no-run --profile gate "${build[@]}" --message-format=json \
  | tee >(jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' >&2) \
  | jq -r 'select(.reason == "compiler-artifact" and .executable != null)
      | [.target.name, (.target.kind | join(",")), .executable] | @tsv' >"$artifacts"
binary() { # <target name> <target kind>
  awk -F'\t' -v n="$1" -v k="$2" '$1 == n && $2 == k { path = $3 } END { print path }' "$artifacts"
}

step "lib tests"
for package in "${libs[@]}"; do
  lib_bin="$(binary "${package//-/_}" lib)"
  [ -x "$lib_bin" ] || { echo "smoke: no lib test binary for $package" >&2; exit 1; }
  run_with_deadline "$KRUSTY_TEST_TIMEOUT_SECONDS" "$lib_bin" -q
done

if [ "${#e2e_modules[@]}" -gt 0 ]; then
  step "changed e2e modules: ${e2e_modules[*]}"
  run_with_deadline "$KRUSTY_E2E_TIMEOUT_SECONDS" "$(binary e2e test)" "${e2e_modules[@]}"
fi

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

[ -n "$lanes" ] || step "no box lane is affected by this change"
[ -z "$lanes" ] || bin="$(binary conformance test)"
for lane in $lanes; do
  run_lane "$lane"
done

step "passed"
