#!/usr/bin/env bash
# Builds one corpus project twice from a clean checkout, with kotlinc (its own Gradle build) and
# with krusty (the same build with the krusty Gradle plugin applied by an init script), then
# compares the class files of every Kotlin/JVM compile task. A warm-up build first downloads what
# both need, and both measured builds run offline. After each measured build, an unmeasured test
# build runs the project's own JVM tests on the classes it compiled, and their outcomes are compared.
#
#   run.sh <name> <work dir> <krusty binary> <krusty Gradle plugin repository> <plugin version>
#
# A third build, in the krusty plugin's verify mode, compiles each compilation with krusty on its own
# against kotlinc's classes of what it depends on, and records every error krusty reports.
#
# Writes <work dir>/out/<name>/{kotlinc,krusty} (the measured builds' class trees),
# <work dir>/out/<name>/{kotlinc,krusty}-tests (JUnit reports), <work dir>/logs/<name>.* (each build's
# log, compile-task and test-task inventory, task times and metrics, and what each test build changed
# in the measured outputs) and <work dir>/out/<name>/compare.json. Compiler arguments are never
# captured or replayed.

# Setup fails fast: a failed fetch, checkout, patch or version-pin rewrite stops the run before
# anything is measured. Only the two Gradle builds' exit statuses are captured rather than fatal.
# A failed reference build fails the run once everything has been compared.
set -euo pipefail

name=$1
work=$(mkdir -p "$2" && cd "$2" && pwd)
krusty=$3
plugin_repository=$4
plugin_version=$5
here=$(cd "$(dirname "$0")" && pwd)
projects=${CORPUS_PROJECTS:-$here/projects.tsv}

row=$(awk -F'\t' -v name="$name" '$1 == name' "$projects")
if [ -z "$row" ]; then
  echo "unknown corpus project: $name" >&2
  exit 2
fi
# A trailing lane (`lsp`, `compile`, or `compile,lsp`) is not an Android variant. `read` keeps the
# rest of the line in its last variable, so leaving the lane on the row stores `compile` as the
# variant, or joins `demoDebug` and `compile,lsp` with a tab.
lane=${row##*$'\t'}
case $lane in
  lsp|compile|compile,lsp) row=${row%$'\t'"$lane"} ;;
esac
if [ "$lane" = lsp ]; then
  echo "$name is in the language-server metric set; this corpus does not build it" >&2
  exit 2
fi
# A Bazel project (one with a `bazel/<name>` target set) builds through `bazel.sh`.
if [ -d "${CORPUS_BAZEL:-$here/bazel}/$name" ]; then
  exec "$here/bazel.sh" "$name" "$work" "$krusty"
fi
IFS=$'\t' read -r _ _ _ gradle_root jdk _ android_variant <<<"$row"
checkout=$work/src/$name
logs=$work/logs
mkdir -p "$logs"
"$here/checkout.sh" "$name" "$checkout"

java_home_var=JAVA_HOME_${jdk}_X64
export JAVA_HOME=${!java_home_var:-$JAVA_HOME}
export LANG=C.UTF-8 LC_ALL=C.UTF-8
if [ "${android_variant:--}" != - ]; then
  export CORPUS_ANDROID_VARIANT=$android_variant
fi
# A project too large to build whole lists its target set in gradle/<name>.targets: the Gradle tasks
# whose compiles, with everything they depend on, are compared, and the test tasks that are run.
targets=${CORPUS_TARGETS_DIR:-$here/gradle}/$name.targets
if [ -f "$targets" ]; then
  export CORPUS_TARGETS=$targets
else
  unset CORPUS_TARGETS
fi
. "$here/toolchains.sh"

# One build of the Gradle root. The init scripts read every project of the build, so the
# configuration cache and isolated projects are off. The Gradle exit status is left in `status`.
# `measure.py` records the build's wall time, CPU time and peak memory, daemons included, and stops
# the daemons it leaves behind; the init script records when each task ran. Every build but the
# warm-up runs offline.
build() {
  local mode=$1
  shift
  local root=$checkout/$gradle_root
  local manifest=$logs/$name.$mode.compilations.tsv
  local start code=0 offline=()
  local deps=$logs/$name.$mode.deps.tsv
  [ "$mode" = warmup ] || offline=(--offline)
  rm -f "$manifest" "$deps"
  start=$(date +%s)
  (
    cd "$root"
    CORPUS_COMPILATIONS=$manifest CORPUS_TASK_TIMES=$logs/$name.$mode.tasks.tsv \
      CORPUS_TASK_DEPS=$deps \
      python3 "$here/measure.py" "$logs/$name.$mode.metrics.json" \
      timeout 5400 ./gradlew --no-daemon -I "$here/corpus.init.gradle" \
      "${toolchains[@]}" "${offline[@]}" "$@" corpusCompile --continue --rerun-tasks --no-configure-on-demand \
      --no-configuration-cache -Dorg.gradle.isolated-projects=false
  ) >"$logs/$name.$mode.log" 2>&1 || code=$?
  echo "$name $mode exit $code seconds $(($(date +%s) - start))" >>"$logs/STATUS"
  status=$code
}

# The project's own JVM tests (`corpusTest`), run on the classes the measured build of the same mode
# compiled, which `snapshot --keep` has already saved and left in place. Every test runs again, since
# the init script reuses no test result. Not measured, and online: the warm-up downloads only what
# compiling needs, not a test runtime. `measure.py` still stops the daemons it leaves, so the next
# measured build starts without them. The exit status is left in `test_status` (124 when it ran out
# of time); the test-task inventory and the JUnit reports of the tasks it lists go to
# out/<mode>-tests. Whether the tests really ran on the measured classes is checked by `release`.
run_tests() {
  local mode=$1
  shift
  local root=$checkout/$gradle_root
  local manifest=$logs/$name.$mode.tests.tsv
  local start code=0
  rm -f "$manifest"
  start=$(date +%s)
  (
    cd "$root"
    CORPUS_TESTS=$manifest python3 "$here/measure.py" "$logs/$name.$mode.tests.metrics.json" \
      timeout "${CORPUS_TEST_TIMEOUT:-3600}" ./gradlew --no-daemon -I "$here/corpus.init.gradle" \
      "${toolchains[@]}" "$@" corpusTest --continue --no-configure-on-demand --no-configuration-cache \
      -Dorg.gradle.isolated-projects=false
  ) >"$logs/$name.$mode.tests.log" 2>&1 || code=$?
  echo "$name $mode tests exit $code seconds $(($(date +%s) - start))" >>"$logs/STATUS"
  local reports=$work/out/$name/$mode-tests
  if [ -f "$manifest" ]; then
    cut -f1,2 "$manifest" >"$logs/$name.$mode.test-reports.tsv"
    python3 "$here/outputs.py" "$logs/$name.$mode.test-reports.tsv" "$root" "$reports"
  else
    rm -rf "$reports"
    mkdir -p "$reports"
  fi
  test_status=$code
}

# A build's compile outputs copied to out/<mode>: the directories the build's own compile-task
# inventory lists. Without `--keep` they are then removed from the checkout, so the next build starts
# without them; with it they stay for the test build of the same mode, and `release` removes them
# afterwards. A broken inventory or snapshot fails the run.
snapshot() {
  local mode=$1
  local manifest=$logs/$name.$mode.compilations.tsv
  if [ ! -f "$manifest" ]; then
    rm -rf "$work/out/$name/$mode"
    mkdir -p "$work/out/$name/$mode"
  elif [ "${2:-}" = --keep ]; then
    python3 "$here/outputs.py" --keep "$manifest" "$checkout/$gradle_root" "$work/out/$name/$mode"
  else
    python3 "$here/outputs.py" "$manifest" "$checkout/$gradle_root" "$work/out/$name/$mode"
  fi
}

# Removes the outputs `snapshot --keep` left for the test build, and lists in
# <logs>/<name>.<mode>.changed.tsv every file under them the test build wrote, rewrote or deleted.
# The snapshot stays the measured build's classes either way; a listed change means the tests did not
# run on them, and the comparison reports that mode's tests as unavailable.
release() {
  local mode=$1
  local manifest=$logs/$name.$mode.compilations.tsv
  local changes=$logs/$name.$mode.changed.tsv
  if [ -f "$manifest" ]; then
    python3 "$here/outputs.py" --clear "$manifest" "$checkout/$gradle_root" "$work/out/$name/$mode" "$changes"
  else
    : >"$changes"
  fi
}

# The warm-up build downloads everything the build needs (dependencies, Gradle and compiler plugins,
# toolchains) and is not compared; its cost goes unrecorded and its outputs are dropped. The two
# measured builds then run offline, so neither pays for the network and both start from the same
# caches. A warm-up that could not download something (a repository or the plugin portal refused
# it) stops the run: the offline builds cannot fetch it later, and a retry would only extend a block
# the repository placed on this host. Any other warm-up failure is the build's own, which the
# reference build then reports. Gradle reports a refused download as "not found" whichever
# repository refused it, so before stopping, the repositories are probed and their HTTP status is
# recorded in <name>.warmup.http.tsv and on stderr: a dead host (000, 5xx) and a throttled one (429)
# then read differently from a missing artifact.
unreachable='Could not (resolve|GET|HEAD)|could not resolve plugin artifact|Connection reset|timed out|Name or service not known|No such host'
# Prints "<status> <url>" for each repository the warm-up log names, and for the default
# ones (CORPUS_PROBE_URLS overrides those, for hosts a test must not reach).
probe_repositories() {
  local url code
  local defaults=${CORPUS_PROBE_URLS-https://repo.maven.apache.org/maven2/ https://maven.google.com/ https://plugins.gradle.org/m2/}
  for url in $(grep -oE 'maven\(https?://[^)]+\)' "$logs/$name.warmup.log" | sed -E 's/^maven\(|\)$//g' | sort -u) $defaults; do
    code=$(curl -s -o /dev/null -I -w '%{http_code}' --max-time 20 "$url") || true
    echo "$code $url" | tee -a "$logs/$name.warmup.http.tsv" >&2
  done
}
build warmup
if [ "$status" -ne 0 ] && grep -qE "$unreachable" "$logs/$name.warmup.log"; then
  probe_repositories
  echo "$name: the warm-up could not download what the build needs; see $name.warmup.log" >&2
  exit 1
fi
snapshot warmup
rm -rf "$work/out/$name/warmup" "$logs/$name.warmup.metrics.json" "$logs/$name.warmup.tasks.tsv"
CORPUS_PLUGIN_SURVEY=$logs/$name.plugins.tsv build kotlinc
reference=$status
if [ ! -f "$logs/$name.kotlinc.compilations.tsv" ]; then
  echo "$name: the reference build recorded no compile-task inventory" >&2
  exit 1
fi
snapshot kotlinc --keep
run_tests kotlinc
kotlinc_tests=$test_status
release kotlinc
# With `$CORPUS_PERF` set (the `perf` command, allowed to sample every process), the krusty
# build is sampled system-wide at a low rate, and the krusty processes' call stacks are folded into
# <logs>/<name>.krusty.folded; with `$CORPUS_FLAMEGRAPH` (inferno's `inferno-flamegraph`) also drawn as
# <logs>/<name>.krusty.svg. Sampling 99 times a second costs the build next to nothing. A previous
# run's samples and graph are removed first, so a run that is not sampled, or samples nothing,
# publishes none.
rm -f "$logs/$name.krusty.folded" "$logs/$name.krusty.svg" "$logs/$name.perf.log"
if [ -n "${CORPUS_PERF:-}" ]; then
  $CORPUS_PERF record -a -g -F 99 -o "$work/$name.perf.data" >"$logs/$name.perf.log" 2>&1 &
  perf_pid=$!
fi
KRUSTY_PLUGIN_REPOSITORY=$plugin_repository KRUSTY_PLUGIN_VERSION=$plugin_version \
  build krusty -I "$here/krusty.init.gradle" -Pkrusty.binary="$krusty"
if [ -n "${CORPUS_PERF:-}" ]; then
  kill -INT "$perf_pid"
  wait "$perf_pid" || true
  $CORPUS_PERF script -i "$work/$name.perf.data" --comms krusty -F comm,ip,sym,dso 2>>"$logs/$name.perf.log" |
    python3 "$here/hotspots.py" fold >"$logs/$name.krusty.folded"
  rm -f "$work/$name.perf.data"
  if [ -n "${CORPUS_FLAMEGRAPH:-}" ] && [ -s "$logs/$name.krusty.folded" ]; then
    "$CORPUS_FLAMEGRAPH" --title "$name: krusty" --width 1600 "$logs/$name.krusty.folded" \
      >"$logs/$name.krusty.svg"
  fi
fi
snapshot krusty --keep
KRUSTY_PLUGIN_REPOSITORY=$plugin_repository KRUSTY_PLUGIN_VERSION=$plugin_version \
  run_tests krusty -I "$here/krusty.init.gradle" -Pkrusty.binary="$krusty"
krusty_tests=$test_status
release krusty
# The verify build compiles every compilation with kotlinc, as the reference build does, and krusty
# compiles each one beside it against kotlinc's classes of the compilations it depends on: the
# plugin's `krustyVerify` mode, which naming that task turns on. A krusty failure therefore never
# keeps a dependent compilation from being compiled, and each compilation's record holds every
# error krusty reported, not only the first error of the first failure. Only the picked compilations
# are verified: `corpusCompile` depends on their verify compiles, and the plugin's report task, which
# would verify every compilation of every module and variant, is excluded. Its kotlinc classes and
# its cost are not compared.
rm -f "$logs/$name.verify.records.tsv"
KRUSTY_PLUGIN_REPOSITORY=$plugin_repository KRUSTY_PLUGIN_VERSION=$plugin_version \
  CORPUS_VERIFY_RECORDS=$logs/$name.verify.records.tsv \
  build verify --offline -I "$here/krusty.init.gradle" -Pkrusty.binary="$krusty" krustyVerify -x krustyVerify
verified=$status
snapshot verify
rm -rf "$work/out/$name/verify" "$logs/$name.verify.metrics.json" "$logs/$name.verify.tasks.tsv"
python3 "$here/compare.py" "$work" "$name" "$reference" "$kotlinc_tests" "$krusty_tests" "$verified"
# A failed reference build is still compared as far as it got, then fails the run: every project is
# meant to build with kotlinc, and a project that does not measures only what its build reached.
if [ "$reference" -ne 0 ]; then
  echo "$name: the reference kotlinc build failed (exit $reference); see $name.kotlinc.log" >&2
  exit 1
fi
# What krusty could not build is a result, not a failure of the run: the comparison above has
# recorded every compilation it missed and why, and the report carries them.
