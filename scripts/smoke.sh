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
#   5. the e2e modules (`tests/*_e2e.rs`) this change touches, and those that name a plugin whose
#      source (`src/plugins/<name>/`) it touches, then the build-tools packaging
#      (`scripts/package-build-tools.sh`: compile, test, jar twice) for every supported Kotlin
#      release when it touches `tools/krusty-build-tools` or that script;
#   6. the conformance entries CI runs for the lanes this change can affect (see select_lanes), in
#      parallel jobs, on the newest Kotlin version, and on the others too if it fails: per lane
#      (JVM, Native, wasm-js, wasm-wasi) an even sample of the box corpus and the owned regressions
#      (`tests/box_regressions/`), the non-box conformance (CLI and bytecode corpora), the focused
#      Native and Wasm tests, and the diagnostics lane when this change touches its expectations.
#
# Every test process runs under the harness's configurable deadlines (scripts/test-gate-defaults.sh).
#
# Knobs: KRUSTY_SMOKE_BOX_LIMIT (corpus cases sampled per lane, default 150; 0 runs the owned
# regressions only), KRUSTY_SMOKE_LANES (overrides the selected lanes, e.g. "jvm native wasm-js
# wasm-wasi"; empty runs none), KRUSTY_SMOKE_VERSIONS (default: newest, then the rest on failure;
# "all" runs every version, or list versions), KRUSTY_SMOKE_JOBS (parallel jobs, default nproc),
# KRUSTY_SMOKE_DIAGNOSTICS=1 (always run the diagnostics lane),
# KRUSTY_JDK_CACHE (where missing JDK 8, 11, 17, 21 and 25 are downloaded),
# KRUSTY_SMOKE_SKIP_CLIPPY=1 (skip the clippy run, the slowest static check: it lints the whole
# workspace from a clean target of its own).
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

# Every path the branch's own commits touch, including a deletion and both sides of a rename: what
# a change can affect, which selects the lanes and checks. A path that no longer exists still changed
# the effective inputs (a deleted expectation list), so it is not filtered out.
affected() {
  git log --first-parent --no-merges --no-renames --name-only --format= "$fork..HEAD" -- "$@" | sort -u
}

# The affected paths that still exist at HEAD: the executable inputs (E2E modules, owned regressions)
# a step can run.
changed() {
  affected "$@" | while IFS= read -r file; do [ -e "$file" ] && printf '%s\n' "$file"; done
}

# The box lanes a change can affect. A backend crate, or its expected-failure lists, selects its own
# lanes; the shared KLIB lowering selects the KLIB lanes; the class-file layer selects JVM. Any other
# compiler source, a shared regression, a conformance script, the harness, or the build selects
# every lane. This script, its test, the workflows and documentation select none.
select_lanes() {
  local file jvm=0 native=0 wasm=0
  while IFS= read -r file; do
    case "$file" in
      crates/krusty-jvm/* | src/jvm/* | tests/box_expected_failures/jvm*) jvm=1 ;;
      crates/krusty-native/* | tests/box_expected_failures/native* | tests/klib_uncovered_implementations/native/*) native=1 ;;
      crates/krusty-wasm/* | tests/box_expected_failures/wasm*) wasm=1 ;;
      src/klib_lowering/* | src/klib/*) native=1 wasm=1 ;;
      *.md | docs/* | scripts/smoke.sh | scripts/test-smoke-selection.sh | .github/*) ;;
      src/* | crates/* | tests/* | scripts/* | build.rs | Cargo.toml | Cargo.lock | .cargo/* | justfile)
        jvm=1 native=1 wasm=1 ;;
    esac
  done < <(affected)
  local selected=()
  [ "$jvm" = 0 ] || selected+=(jvm)
  [ "$native" = 0 ] || selected+=(native)
  [ "$wasm" = 0 ] || selected+=(wasm-js wasm-wasi)
  printf '%s\n' "${selected[*]}"
}
lanes="${KRUSTY_SMOKE_LANES-$(select_lanes)}"

# The e2e modules a change can affect: the modules it changes, and the modules that name a plugin
# whose source it changes. A plugin is the directory (or file stem) directly under src/plugins/ that
# a changed path is in; a module covers it when its file, or any file of its directory, names the
# plugin as a word. A source file directly under src/plugins/ belongs to no plugin.
e2e_modules_for() {
  {
    changed 'tests/*_e2e.rs' 'tests/*_e2e/*.rs' | sed -E 's#^tests/([^/]+_e2e)(/.*)?\.rs$#\1#; s#^tests/([^/]+_e2e)/.*#\1#'
    affected 'src/plugins/*/*' | sed -E 's#^src/plugins/([^/]+)/.*#\1#' | sort -u | while IFS= read -r plugin; do
      grep -liw -- "$plugin" tests/*_e2e.rs || true
      for dir in tests/*_e2e/; do
        ! grep -rqiw -- "$plugin" "$dir" || printf '%s\n' "${dir%/}.rs"
      done
    done | sed -E 's#^tests/##; s#\.rs$##'
  } | sort -u | sed 's#$#::#'
}
e2e_modules=()
while IFS= read -r module; do
  e2e_modules+=("$module")
done < <(e2e_modules_for)
# The selection alone, for the scenario test: no checks, no build.
if [ "${KRUSTY_SMOKE_PRINT_E2E:-}" = 1 ]; then
  printf '%s\n' "${e2e_modules[@]}"
  exit 0
fi

# The selection alone, for the scenario test: no checks, no build.
if [ "${KRUSTY_SMOKE_PRINT_LANES:-}" = 1 ]; then
  printf '%s\n' "$lanes"
  exit 0
fi

step "attribution and outcome lists ($fork..HEAD)"
bash scripts/check-attribution.sh "$fork..HEAD"
bash scripts/check-outcome-lists.sh "$fork" HEAD

step "rustfmt"
just fmt-check
step "legacy baseline"
just legacy-baseline-check
# The legacy cap is global: a branch that fits under it on its own base can exceed it once the
# merges that landed since are combined with it, which is what the merge queue builds. Check the
# merge of this branch with origin/master in a throwaway worktree.
step "legacy baseline on the merge with origin/master"
merge_tree="$(git merge-tree --write-tree HEAD origin/master)" || {
  echo "smoke: this branch does not merge cleanly into origin/master; resolve the conflict first" >&2
  exit 1
}
merge_commit="$(git commit-tree "$merge_tree" -p HEAD -p origin/master -m 'smoke merge')"
merge_dir="$(mktemp -d)"
git worktree add --quiet --detach "$merge_dir" "$merge_commit"
trap 'git worktree remove --force "$merge_dir" >/dev/null 2>&1 || true' EXIT
(cd "$merge_dir" && just legacy-baseline-check)
git worktree remove --force "$merge_dir"
trap - EXIT
if [ "${KRUSTY_SMOKE_SKIP_CLIPPY:-}" != 1 ] && [ -n "$(affected '*.rs')" ]; then
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
  # An e2e module that spawns the CLI runs the gate build of it, as run-tests.sh passes it.
  if [ -z "${KRUSTY_BIN:-}" ]; then
    cargo build --profile gate -p krusty-cli --bin krusty
    target_root="${CARGO_TARGET_DIR:-$repo_root/target}"
    export KRUSTY_BIN="$target_root/gate/krusty"
  fi
  run_with_deadline "$KRUSTY_E2E_TIMEOUT_SECONDS" "$(binary e2e test)" "${e2e_modules[@]}"
fi

# CI builds and tests the Build Tools API implementation once per supported Kotlin release, and a
# release's API can differ from the newest one's (an option constructor, a missing option), so a
# change there runs the same packaging for each release.
if [ -n "$(changed 'tools/krusty-build-tools/*' 'scripts/package-build-tools.sh')" ]; then
  if [ -z "${KRUSTY_BIN:-}" ]; then
    cargo build --profile gate -p krusty-cli --bin krusty
    export KRUSTY_BIN="${CARGO_TARGET_DIR:-$repo_root/target}/gate/krusty"
  fi
  packaged="$(mktemp -d)"
  while IFS= read -r release; do
    step "build-tools packaging for Kotlin $release"
    scripts/package-build-tools.sh 0.0.0-smoke "$release" "$KRUSTY_BIN" "$packaged/$release"
  done < <(just kotlin-versions)
  rm -rf "$packaged"
fi

# The Analysis API adapter is a Gradle build whose tests drive real KSP2 sessions, and the fixtures
# under tests/psi_ranges are shared with krusty's parser tests; CI runs it as its own job.
if [ -n "$(changed 'tools/krusty-analysis-api/*' 'tests/psi_ranges/*')" ]; then
  step "analysis-api tests"
  run_with_deadline "${KRUSTY_ANALYSIS_API_TIMEOUT_SECONDS:-1800}" \
    tools/krusty-analysis-api/gradlew --no-daemon -p tools/krusty-analysis-api test
fi

if [ -z "$lanes" ]; then
  step "no conformance lane is affected by this change"
  step "passed"
  exit 0
fi
bin="$(binary conformance test)"
# The CLI the non-box corpus drives is the one built beside the binary, as in CI.
sibling="$(cd "$(dirname "$bin")" && pwd)/krusty"
[ -x "$sibling" ] && export KRUSTY_BIN="${KRUSTY_BIN:-$sibling}"

# Every conformance entry CI runs, for the newest Kotlin version first (then the others if it fails), as parallel jobs:
#   <v> <lane>        the sampled corpus slice, then the owned regressions (run_lane)
#   <v> nonbox        scripts/conformance-regressions.sh: the JVM-owned non-box tests, the CLI corpus
#                     and the bytecode corpora against the reference compiler
#   <v> native-focus  scripts/target-conformance-regressions.sh native
#   <v> wasm-focus    scripts/target-conformance-regressions.sh wasm
#   <max> diagnostics the diagnostics test data lane; a full run compiles every case, so it runs
#                     when the branch changes its expectations or KRUSTY_SMOKE_DIAGNOSTICS=1
# Deadlines and parallelism: see KRUSTY_SMOKE_JOBS and the *_TIMEOUT_SECONDS defaults below.
# A job's output goes to a log that is printed only when the job fails. All jobs run to the end so
# one push fixes every failure the try would have reported.
# The newest version first; the others only when it fails (the failure may be version-specific, or
# fixed for it and not for them) or when KRUSTY_SMOKE_VERSIONS=all (or a list) asks for them.
newest="$(just max-version)"
all_versions="$(just kotlin-versions | tr '\n' ' ')"
case "${KRUSTY_SMOKE_VERSIONS:-}" in
  "") first="$newest" rest="" ;;
  all) first="$newest" rest="$(printf '%s\n' $all_versions | grep -vxF "$newest" | tr '\n' ' ')" ;;
  *) first="$KRUSTY_SMOKE_VERSIONS" rest="" ;;
esac
cores="$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN)"
# Every job is itself multi-threaded and several share one machine, so half the cores run at once
# and the deadlines are CI's, not the plain gate's two minutes.
jobs_max="${KRUSTY_SMOKE_JOBS:-$(( cores / 2 > 2 ? cores / 2 : 2 ))}"
export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_SMOKE_CONFORMANCE_TIMEOUT_SECONDS:-600}"
export KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_SMOKE_TARGET_TIMEOUT_SECONDS:-1800}"
export KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_SMOKE_TARGET_TIMEOUT_SECONDS:-1800}"

# The JDKs the CLI corpus names (8, 11, 17, 21) and the box runner's 25, downloaded into
# target/cache/jdks when no installed home provides them (scripts/provision-jdks.sh).
# The helper's status is checked before any export, so a failed download leaves nothing half set.
jdk_homes="$(scripts/provision-jdks.sh 8 11 17 21 25)" || exit 1
while IFS='=' read -r name home; do
  export "$name=$home"
done <<<"$jdk_homes"

export KRUSTY_BOX_CASE_TIMEOUT_MS="${KRUSTY_BOX_CASE_TIMEOUT_MS:-8000}"
export KRUSTY_REQUIRE_SERIALIZATION_CONFORMANCE=1
export KRUSTY_BOX_TIMEOUT_MS="${KRUSTY_BOX_TIMEOUT_MS:-60000}"
export KRUSTY_KLIB_REFERENCE_CACHE="${KRUSTY_KLIB_REFERENCE_CACHE:-$PWD/target/cache/klib-references}"

diagnostics=0
[ "${KRUSTY_SMOKE_DIAGNOSTICS:-}" = 1 ] && diagnostics=1
if [ -n "$(affected 'tests/diagnostics_false_accepts/**' 'tests/diagnostics_not_scored/**' 'tests/kotlin_diagnostics_jvm_conformance.rs')" ]; then
  diagnostics=1
fi

# One lane of one version: the sampled corpus slice, then owned regressions. JVM, wasm-js and
# wasm-wasi run every owned regression; their inventory keeps only the ones that apply to the
# release under test. Native runs only the regressions this branch changes, because each
# multi-module one first runs kotlinc-native as its oracle, which takes minutes for the whole set;
# a changed regression outside the release under test selects nothing there. A narrowed run checks
# the expected-failure lists over exactly the cases it scanned.
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
    echo "$lane: $limit sampled corpus cases"
    run_with_deadline "$deadline" env "${require[@]}" "KRUSTY_${infix}BOX_LIMIT=$limit" \
      "$bin" --exact "$test" --nocapture
  fi
  if [ "$lane" != native ]; then
    echo "$lane: owned regressions"
    run_with_deadline "$deadline" env "${require[@]}" "KRUSTY_${infix}BOX_ONLY=/tests/box_regressions/" \
      "$bin" --exact "$test" --nocapture
    return
  fi
  for only in "${regressions[@]}"; do
    echo "$lane: changed regression $only"
    run_with_deadline "$deadline" env "${require[@]}" "KRUSTY_${infix}BOX_ONLY=$only" \
      "$bin" --exact "$test" --nocapture
  done
}

# One job, in its own subshell so its version's environment reaches nothing else.
run_job() {
  local v=$1 kind=$2
  export KRUSTY_LANGUAGE_VERSION="$v" KRUSTY_KOTLINC="${kotlinc_of[$v]}" KRUSTY_KOTLIN_BOX_DIR="${box_of[$v]}"
  case "$kind" in
    nonbox) bash scripts/conformance-regressions.sh "$bin" "$v" ;;
    native-focus) KRUSTY_KOTLIN_NATIVE="${native_of[$v]}" bash scripts/target-conformance-regressions.sh native "$bin" "$v" ;;
    wasm-focus) bash scripts/target-conformance-regressions.sh wasm "$bin" "$v" ;;
    diagnostics) bash scripts/diagnostics-conformance.sh "$bin" "$v" ;;
    native) KRUSTY_KOTLIN_NATIVE="${native_of[$v]}" run_lane native ;;
    *) run_lane "$kind" ;;
  esac
}

logdir="$(mktemp -d "${TMPDIR:-/tmp}/krusty-smoke.XXXXXX")"
trap 'rm -rf "$logdir" "$artifacts"' EXIT
declare -A job_pid job_began kotlinc_of box_of native_of
failed=()
order=()

start_job() {
  local name="$1 $2"
  # Count the live jobs rather than keeping a counter: `wait -n` also returns for a child that is
  # not one of the jobs, which let a counter drift below the real number running.
  while [ "$(jobs -rp | wc -l)" -ge "$jobs_max" ]; do sleep 1; done
  # The exit status goes to a file, so the result does not depend on when the job is reaped.
  # The file also records when the job ended, so the report shows how many ran at once.
  ( trap 'echo $? >"$logdir/${name// /_}.rc"; echo $((SECONDS - start)) >"$logdir/${name// /_}.end"' EXIT
    run_job "$1" "$2" ) >"$logdir/${name// /_}.log" 2>&1 &
  job_pid[$name]=$!
  job_began[$name]=$((SECONDS - start))
  order+=("$name")
  step "started: $name ($(jobs -rp | wc -l) of $jobs_max running)"
}

# Provision the given versions, run all their jobs in parallel, and report each result.
run_versions() {
  local v lane name kc bc kn
  order=()
  for v in "$@"; do
    # Each version's exact reference toolchain, provisioned before the jobs share it.
    step "provisioning Kotlin $v"
    # Each step is checked on its own: an assignment's status is not reliably seen by `set -e`.
    kc="$(just kotlinc "$v")" || exit 1
    bc="$(just box-corpus "$v")" || exit 1
    # Every plugin that has a box corpus, as the conformance entries need all of them.
    while read -r plugin; do
      just plugin-corpus "$plugin" "$v" >/dev/null || exit 1
    done < <(just plugin-corpora)
    kotlinc_of[$v]="$kc"
    box_of[$v]="$bc"
    case " $lanes " in
      *" native "*) kn="$(just kotlin-native "$v")" || exit 1; native_of[$v]="$kn" ;;
    esac
  done
  for v in "$@"; do
    for lane in $lanes; do start_job "$v" "$lane"; done
    start_job "$v" nonbox
    case " $lanes " in *" native "*) start_job "$v" native-focus ;; esac
    case " $lanes " in *" wasm-js "*|*" wasm-wasi "*) start_job "$v" wasm-focus ;; esac
    if [ "$diagnostics" = 1 ] && [ "$v" = "$newest" ]; then start_job "$v" diagnostics; fi
  done
  # A run that scheduled nothing must not read as a pass.
  [ "${#order[@]}" -gt 0 ] || { echo "smoke: no conformance job was scheduled" >&2; exit 1; }
  for name in "${order[@]}"; do
    wait "${job_pid[$name]}" 2>/dev/null || true
    if [ "$(cat "$logdir/${name// /_}.rc" 2>/dev/null)" = 0 ]; then
      step "ok: $name (ran ${job_began[$name]}s to $(cat "$logdir/${name// /_}.end")s)"
    else
      step "FAILED: $name (ran ${job_began[$name]}s to $(cat "$logdir/${name// /_}.end" 2>/dev/null || echo ?)s)"
      failed+=("$name")
      echo "----- $name -----" >&2
      tail -n 80 "$logdir/${name// /_}.log" >&2
    fi
  done
  }

run_versions $first
# Only the default falls back; an explicit list runs exactly that list.
if [ "${#failed[@]}" -gt 0 ] && [ -z "${KRUSTY_SMOKE_VERSIONS:-}" ]; then
  rest="$(printf '%s\n' $all_versions | grep -vxF -e "$newest" | tr '\n' ' ')"
  step "newest version failed: checking the other versions too"
fi
[ -z "$rest" ] || run_versions $rest

if [ "${#failed[@]}" -gt 0 ]; then
  printf 'smoke: failed: %s\n' "${failed[@]}" >&2
  exit 1
fi
step "passed"
