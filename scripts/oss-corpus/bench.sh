#!/usr/bin/env bash
# Benchmarks krusty against kotlinc with a warm Gradle daemon on one corpus project: clean builds and
# an edit loop, wall time and peak memory. kotlinc runs the way a developer's build runs it, in the
# Kotlin daemon with incremental compilation; krusty runs through its Gradle plugin.
#
#   bench.sh <name> <work dir> <krusty binary> <krusty Gradle plugin repository> <plugin version>
#
# A krusty probe build (`corpusCompile --continue`, daemon off, online) first finds the compilations
# krusty builds; only those are benchmarked, so both compilers build the same tasks and both builds
# succeed. `bench.py` then runs one session per compiler, each against its own Gradle daemon, and
# writes <work dir>/bench/<name>.json and a Markdown table. See `bench.py` for the scenarios and
# the BENCH_* knobs, and README.md for what the numbers mean.
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
IFS=$'\t' read -r _ _ _ gradle_root jdk _ android_variant <<<"$row"
checkout=$work/src/$name
root=$checkout/$gradle_root
logs=$work/logs
bench=$work/bench
mkdir -p "$logs" "$bench/$name"
rm -f "$bench/$name"/*.json "$bench/$name.json"
"$here/checkout.sh" "$name" "$checkout"

java_home_var=JAVA_HOME_${jdk}_X64
export JAVA_HOME=${!java_home_var:-$JAVA_HOME}
export LANG=C.UTF-8 LC_ALL=C.UTF-8
if [ "${android_variant:--}" != - ]; then
  export CORPUS_ANDROID_VARIANT=$android_variant
fi
export KRUSTY_PLUGIN_REPOSITORY=$plugin_repository KRUSTY_PLUGIN_VERSION=$plugin_version

# The init scripts read every project of the build, so the configuration cache and isolated
# projects are off, as in run.sh.
common=(-I "$here/corpus.init.gradle" --no-configure-on-demand --no-configuration-cache
  -Dorg.gradle.isolated-projects=false)
with_krusty=(-I "$here/krusty.init.gradle" -Pkrusty.binary="$krusty")

probe=$logs/$name.probe
(
  cd "$root"
  CORPUS_COMPILATIONS=$probe.compilations.tsv CORPUS_SOURCES=$probe.sources.tsv \
    CORPUS_TASK_TIMES=$probe.tasks.tsv \
    python3 "$here/measure.py" "$probe.metrics.json" \
    timeout 5400 ./gradlew --no-daemon "${common[@]}" "${with_krusty[@]}" \
    corpusCompile --continue --rerun-tasks
) >"$probe.log" 2>&1 || true
python3 "$here/bench.py" plan "$logs" "$name" "$root" "$bench/$name/plan.json"

# Each session leaves the edited file as it found it and stops its daemons, so the second compiler
# starts from the same sources and pays for its own warm-up. A failed session is recorded in its
# result and the report.
(cd "$root" && python3 "$here/bench.py" session "$bench/$name/plan.json" kotlinc \
  "$bench/$name/kotlinc.json" "${common[@]}") || true
(cd "$root" && python3 "$here/bench.py" session "$bench/$name/plan.json" krusty \
  "$bench/$name/krusty.json" "${common[@]}" "${with_krusty[@]}") || true
python3 "$here/bench.py" report "$bench" "$name"
