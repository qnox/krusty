#!/usr/bin/env bash
# Builds one corpus project twice from a clean checkout, with kotlinc (its own Gradle build) and
# with krusty (the same build with the krusty Gradle plugin applied by an init script), then
# compares the class files of every Kotlin/JVM compile task.
#
#   run.sh <name> <work dir> <krusty binary> <krusty Gradle plugin repository> <plugin version>
#
# Writes <work dir>/out/<name>/{kotlinc,krusty} (class trees), <work dir>/logs/<name>.*.log and
# <work dir>/out/<name>/compare.json. Compiler arguments are never captured or replayed.
set -uo pipefail

name=$1
work=$(mkdir -p "$2" && cd "$2" && pwd)
krusty=$3
plugin_repository=$4
plugin_version=$5
here=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$here/../.." && pwd)

row=$(awk -F'\t' -v name="$name" '$1 == name' "$here/projects.tsv")
if [ -z "$row" ]; then
  echo "unknown corpus project: $name" >&2
  exit 2
fi
IFS=$'\t' read -r _ url commit gradle_root jdk pin <<<"$row"

kotlin=$(sed 's/#.*//' "$repo_root/kotlin-versions" | awk 'NF { print $1 }' | sort -V | tail -n 1)
checkout=$work/src/$name
logs=$work/logs
mkdir -p "$logs"

rm -rf "$checkout"
git init -q "$checkout"
git -C "$checkout" remote add origin "$url"
git -C "$checkout" fetch -q --depth 1 origin "$commit"
git -C "$checkout" checkout -q FETCH_HEAD
if [ -f "$here/patches/$name.patch" ]; then
  git -C "$checkout" apply "$here/patches/$name.patch"
fi
if [ "$pin" != - ]; then
  file=$checkout/${pin%%:*}
  key=${pin#*:}
  case "$file" in
    *.toml) sed -i -E "s/^($key[[:space:]]*=[[:space:]]*)\"[^\"]*\"/\\1\"$kotlin\"/" "$file" ;;
    *.properties) sed -i -E "s/^($key[[:space:]]*=[[:space:]]*).*/\\1$kotlin/" "$file" ;;
    *) echo "unsupported Kotlin version pin: $pin" >&2; exit 2 ;;
  esac
fi

java_home_var=JAVA_HOME_${jdk}_X64
export JAVA_HOME=${!java_home_var:-$JAVA_HOME}
export LANG=C.UTF-8 LC_ALL=C.UTF-8

build() {
  local mode=$1
  shift
  local root=$checkout/$gradle_root
  (
    cd "$root" || exit 1
    find . -path '*/build/classes/kotlin' -prune -exec rm -rf {} + 2>/dev/null
    start=$(date +%s)
    timeout 5400 ./gradlew --no-daemon -I "$here/corpus.init.gradle" "$@" corpusCompile \
      --continue --rerun-tasks --no-configure-on-demand >"$logs/$name.$mode.log" 2>&1
    code=$?
    echo "$name $mode exit $code seconds $(($(date +%s) - start))" >>"$logs/STATUS"
    out=$work/out/$name/$mode
    rm -rf "$out"
    mkdir -p "$out"
    find . -path '*/build/classes/kotlin' -prune -print | while read -r dir; do
      mkdir -p "$out/$dir"
      cp -a "$dir/." "$out/$dir/"
    done
    exit "$code"
  )
}

CORPUS_PLUGIN_SURVEY=$logs/$name.plugins.tsv CORPUS_COMPILATIONS=$logs/$name.compilations.tsv build kotlinc
reference=$?
KRUSTY_PLUGIN_REPOSITORY=$plugin_repository KRUSTY_PLUGIN_VERSION=$plugin_version \
  build krusty -I "$here/krusty.init.gradle" -Pkrusty.binary="$krusty"
python3 "$here/compare.py" "$work" "$name" "$reference"
