#!/usr/bin/env bash
# Builds one corpus project twice from a clean checkout, with kotlinc (its own Gradle build) and
# with krusty (the same build with the krusty Gradle plugin applied by an init script), then
# compares the class files of every Kotlin/JVM compile task.
#
#   run.sh <name> <work dir> <krusty binary> <krusty Gradle plugin repository> <plugin version>
#
# Writes <work dir>/out/<name>/{kotlinc,krusty} (class trees), <work dir>/logs/<name>.*.log and
# <work dir>/out/<name>/compare.json. Compiler arguments are never captured or replayed.

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
# Gradle exit status is left in `status`; a broken inventory or snapshot fails the run.
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
    CORPUS_COMPILATIONS=$manifest timeout 5400 ./gradlew --no-daemon -I "$here/corpus.init.gradle" \
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

CORPUS_PLUGIN_SURVEY=$logs/$name.plugins.tsv build kotlinc
reference=$status
if [ ! -f "$logs/$name.kotlinc.compilations.tsv" ]; then
  echo "$name: the reference build recorded no compile-task inventory" >&2
  exit 1
fi
KRUSTY_PLUGIN_REPOSITORY=$plugin_repository KRUSTY_PLUGIN_VERSION=$plugin_version \
  build krusty -I "$here/krusty.init.gradle" -Pkrusty.binary="$krusty"
python3 "$here/compare.py" "$work" "$name" "$reference"
