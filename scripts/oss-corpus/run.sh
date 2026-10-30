#!/usr/bin/env bash
# Builds one corpus project twice from a clean checkout, with kotlinc (its own Gradle build) and
# with krusty (the same build with the krusty Gradle plugin applied by an init script), then
# compares the class files of every Kotlin/JVM compile task. A warm-up build first downloads what
# both need, and both measured builds run offline.
#
#   run.sh <name> <work dir> <krusty binary> <krusty Gradle plugin repository> <plugin version>
#
# Writes <work dir>/out/<name>/{kotlinc,krusty} (class trees), <work dir>/logs/<name>.* (each build's
# log, compile-task inventory, task times and metrics) and <work dir>/out/<name>/compare.json. Compiler arguments are never captured or replayed.

# Setup fails fast: a failed fetch, checkout, patch or version-pin rewrite stops the run before
# anything is measured. Only the two Gradle builds' exit statuses are captured rather than fatal.
set -euo pipefail

name=$1
work=$(mkdir -p "$2" && cd "$2" && pwd)
krusty=$3
plugin_repository=$4
plugin_version=$5
here=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$here/../.." && pwd)
projects=${CORPUS_PROJECTS:-$here/projects.tsv}

row=$(awk -F'\t' -v name="$name" '$1 == name' "$projects")
if [ -z "$row" ]; then
  echo "unknown corpus project: $name" >&2
  exit 2
fi
IFS=$'\t' read -r _ url commit gradle_root jdk pin android_variant <<<"$row"

kotlin=$(sed 's/#.*//' "$repo_root/kotlin-versions" | awk 'NF { print $1 }' | sort -V | tail -n 1)
if [ -z "$kotlin" ]; then
  echo "kotlin-versions lists no release" >&2
  exit 2
fi
checkout=$work/src/$name
logs=$work/logs
mkdir -p "$logs"

rm -rf "$checkout"
git init -q "$checkout"
git -C "$checkout" remote add origin "$url"
git -C "$checkout" fetch -q --depth 1 origin "$commit"
git -C "$checkout" checkout -q FETCH_HEAD
head=$(git -C "$checkout" rev-parse HEAD)
if [ "$head" != "$commit" ]; then
  echo "$name: checked out $head, not the pinned $commit" >&2
  exit 1
fi
if [ -f "$here/patches/$name.patch" ]; then
  git -C "$checkout" apply "$here/patches/$name.patch"
fi
if [ "$pin" != - ]; then
  python3 "$here/pin.py" "$checkout/${pin%%:*}" "${pin#*:}" "$kotlin"
fi

java_home_var=JAVA_HOME_${jdk}_X64
export JAVA_HOME=${!java_home_var:-$JAVA_HOME}
export LANG=C.UTF-8 LC_ALL=C.UTF-8
if [ "${android_variant:--}" != - ]; then
  export CORPUS_ANDROID_VARIANT=$android_variant
fi

# One build of the Gradle root, then its compile outputs moved to out/<mode>: the directories the
# build's own compile-task inventory lists, so the next build starts without them. The init scripts
# read every project of the build, so the configuration cache and isolated projects are off. The
# Gradle exit status is left in `status`; a broken inventory or snapshot fails the run. `measure.py`
# records the build's wall time, CPU time and peak memory, daemons included, and stops the daemons it
# leaves behind; the init script records when each task ran.
build() {
  local mode=$1
  shift
  local root=$checkout/$gradle_root
  local manifest=$logs/$name.$mode.compilations.tsv
  local start code=0
  rm -f "$manifest"
  start=$(date +%s)
  (
    cd "$root"
    CORPUS_COMPILATIONS=$manifest CORPUS_TASK_TIMES=$logs/$name.$mode.tasks.tsv \
      python3 "$here/measure.py" "$logs/$name.$mode.metrics.json" \
      timeout 5400 ./gradlew --no-daemon -I "$here/corpus.init.gradle" \
      "$@" corpusCompile --continue --rerun-tasks --no-configure-on-demand \
      --no-configuration-cache -Dorg.gradle.isolated-projects=false
  ) >"$logs/$name.$mode.log" 2>&1 || code=$?
  echo "$name $mode exit $code seconds $(($(date +%s) - start))" >>"$logs/STATUS"
  if [ -f "$manifest" ]; then
    python3 "$here/outputs.py" "$manifest" "$root" "$work/out/$name/$mode"
  else
    rm -rf "$work/out/$name/$mode"
    mkdir -p "$work/out/$name/$mode"
  fi
  status=$code
}

# The warm-up build downloads everything the build needs (dependencies, Gradle and compiler plugins,
# toolchains) and is not compared; its cost goes unrecorded and its outputs are dropped. The two
# measured builds then run offline, so neither pays for the network and both start from the same
# caches.
build warmup
rm -rf "$work/out/$name/warmup" "$logs/$name.warmup.metrics.json" "$logs/$name.warmup.tasks.tsv"
CORPUS_PLUGIN_SURVEY=$logs/$name.plugins.tsv build kotlinc --offline
reference=$status
if [ ! -f "$logs/$name.kotlinc.compilations.tsv" ]; then
  echo "$name: the reference build recorded no compile-task inventory" >&2
  exit 1
fi
# With `$CORPUS_PERF` set (the `perf` command, allowed to sample every process), the krusty
# build is sampled system-wide at a low rate, and the krusty processes' call stacks are folded into
# <logs>/<name>.krusty.folded; with `$CORPUS_FLAMEGRAPH` (inferno's `inferno-flamegraph`) also drawn as
# <logs>/<name>.krusty.svg. Sampling 99 times a second costs the build next to nothing.
if [ -n "${CORPUS_PERF:-}" ]; then
  $CORPUS_PERF record -a -g -F 99 -o "$work/$name.perf.data" >"$logs/$name.perf.log" 2>&1 &
  perf_pid=$!
fi
KRUSTY_PLUGIN_REPOSITORY=$plugin_repository KRUSTY_PLUGIN_VERSION=$plugin_version \
  build krusty --offline -I "$here/krusty.init.gradle" -Pkrusty.binary="$krusty"
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
python3 "$here/compare.py" "$work" "$name" "$reference"
