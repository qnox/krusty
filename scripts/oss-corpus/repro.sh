#!/usr/bin/env bash
# Reproduces one corpus compilation locally, the way the nightly's verify build compiles it: the
# project is checked out as the nightly checks it out (pinned commit, patch, Kotlin version pin),
# kotlinc compiles the compilation and everything it depends on, krusty compiles the same request
# beside it against kotlinc's classes, and every error krusty reported is printed.
#
#   repro.sh <name> <compile task path> [<work dir>]
#
# For example `repro.sh ktor :ktor-io:compileKotlinJvm`. The work dir defaults to
# target/oss-corpus-repro. A checkout already there is reused, so a second run after changing krusty
# only rebuilds krusty and the one compilation; delete <work dir>/src/<name> to check it out again.
#
# krusty and its Gradle plugin are built from this repository, unless `$KRUSTY_BINARY` names a
# krusty binary and `$KRUSTY_PLUGIN_REPOSITORY` and `$KRUSTY_PLUGIN_VERSION` name a packaged plugin
# (the nightly's `corpus-tools` artifact holds both). The project's JDK is `$JAVA_HOME_<jdk>_X64`
# when that is set, as on GitHub's runners, and `$JAVA_HOME` otherwise.
#
# Exits 0 when krusty compiled the compilation, 1 when it reported errors or the build failed.
set -euo pipefail

if [ $# -lt 2 ]; then
  echo "usage: $0 <name> <compile task path> [<work dir>]" >&2
  exit 2
fi
name=$1
task=$2
here=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$here/../.." && pwd)
work=$(mkdir -p "${3:-$repo_root/target/oss-corpus-repro}" && cd "${3:-$repo_root/target/oss-corpus-repro}" && pwd)
projects=${CORPUS_PROJECTS:-$here/projects.tsv}

row=$(awk -F'\t' -v name="$name" '$1 == name' "$projects")
if [ -z "$row" ]; then
  echo "unknown corpus project: $name" >&2
  exit 2
fi
lane=${row##*$'\t'}
case $lane in
  lsp|compile|compile,lsp) row=${row%$'\t'"$lane"} ;;
esac
IFS=$'\t' read -r _ _ _ gradle_root jdk _ android_variant <<<"$row"

checkout=$work/src/$name
if [ ! -d "$checkout/.git" ]; then
  "$here/checkout.sh" "$name" "$checkout"
fi

if [ -z "${KRUSTY_BINARY:-}" ]; then
  (cd "$repo_root" && cargo build --profile gate -p krusty-cli --bin krusty)
  KRUSTY_BINARY=$repo_root/target/gate/krusty
fi
if [ -z "${KRUSTY_PLUGIN_REPOSITORY:-}" ]; then
  KRUSTY_PLUGIN_VERSION=$(cd "$repo_root" && just version)
  plugin=$work/plugin/$KRUSTY_PLUGIN_VERSION
  if [ ! -d "$plugin" ]; then
    "$repo_root/scripts/package-gradle-plugin.sh" "$KRUSTY_PLUGIN_VERSION" "$work/plugin/dist"
    unzip -q "$work/plugin/dist/krusty-gradle-plugin-$KRUSTY_PLUGIN_VERSION.zip" -d "$plugin"
  fi
  KRUSTY_PLUGIN_REPOSITORY=$plugin/krusty-gradle-plugin-$KRUSTY_PLUGIN_VERSION/repository
fi
export KRUSTY_PLUGIN_REPOSITORY KRUSTY_PLUGIN_VERSION

java_home_var=JAVA_HOME_${jdk}_X64
export JAVA_HOME=${!java_home_var:-${JAVA_HOME:-}}
export LANG=C.UTF-8 LC_ALL=C.UTF-8
if [ "${android_variant:--}" != - ]; then
  export CORPUS_ANDROID_VARIANT=$android_variant
fi

# Naming `krustyVerify` turns the plugin's verify mode on; excluding it keeps the report task, which
# would verify every compilation of the build, from running. Only the one verify compile runs.
records=$work/$name.verify.records.tsv
code=0
(
  cd "$checkout/$gradle_root"
  CORPUS_VERIFY_RECORDS=$records ./gradlew -I "$here/corpus.init.gradle" -I "$here/krusty.init.gradle" \
    -Pkrusty.binary="$KRUSTY_BINARY" krustyVerify -x krustyVerify "${task}KrustyVerify" \
    --no-configuration-cache -Dorg.gradle.isolated-projects=false
) || code=$?

status=0
python3 - "$records" "$checkout" "$task" "$here" <<'EOF' || status=$?
import sys
sys.path.insert(0, sys.argv[4])
import compare

records, checkout, task = sys.argv[1:4]
found = compare.verified(records, checkout).get(task)
if found is None:
    print(f'{task}: the verify build has no compilation by that path', file=sys.stderr)
    sys.exit(1)
print(f"{task}: {found['status']}")
for error in found['errors']:
    print(f'  {error}')
sys.exit(0 if found['status'] == 'compiled' else 1)
EOF
if [ "$code" -ne 0 ] && [ "$status" -eq 0 ]; then
  echo "the Gradle build failed (exit $code)" >&2
  exit 1
fi
exit "$status"
