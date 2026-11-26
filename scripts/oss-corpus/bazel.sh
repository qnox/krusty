#!/usr/bin/env bash
# Builds one Bazel corpus project twice from a clean checkout, with the project's own Kotlin compiler
# and with krusty, then compares the class files of every Kotlin/JVM compile action. `run.sh` hands a
# project here when `bazel/<name>/targets` exists.
#
#   bazel.sh <name> <work dir> <krusty binary>
#
# `bazel/<name>/targets` lists the Bazel targets to build, and `bazel/<name>/integration` names the
# directory (relative to this repository's root) whose patches route the project's Kotlin compile actions to krusty
# (for intellij-community, `tools/krusty-bazel/intellij`). Those patches are applied once, after
# checkout: with `KRUSTY_BINARY` unset they change nothing, so the reference build is the project's
# own, and with it set every eligible action is compiled by krusty (`KrustyCompile`) while the rest
# stay on the project's builder (`JvmCompile`). An optional `bazel/<name>/checkouts` lists, one per
# line and tab-separated, further repositories the build reads (`<directory> <git URL> <commit>`),
# each checked out at its pinned commit into that directory of the checkout first: intellij-community
# reads the project model of the Android plugin, a repository of its own (`getPlugins.sh`).
#
# A warm-up (`build --nobuild`) fetches every repository both builds need and is not compared. Each
# measured build then starts from `bazel clean`, without a disk cache and with downloads disabled, so
# neither reuses the other's actions nor pays for the network. Each writes its execution log, from
# which `bazel_compare.py snapshot` lists the compile actions (`<name>.<mode>.compilations.tsv`) and
# copies their jars out of the execution root (`bazel info execution_root`, which the project's own
# convenience symlinks need not name `bazel-out`) to out/<name>/<mode>. `bazel_compare.py compare` writes out/<name>/compare.json in
# the shape `compare.py` writes for a Gradle project; there is no verify or test build. A failed
# reference build fails the run once that comparison is written.
set -euo pipefail

name=$1
work=$(mkdir -p "$2" && cd "$2" && pwd)
krusty=$3
here=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$here/../.." && pwd)
project=${CORPUS_BAZEL:-$here/bazel}/$name

if [ ! -f "$project/targets" ] || [ ! -f "$project/integration" ]; then
  echo "$name: no Bazel target set (bazel/$name/targets and bazel/$name/integration)" >&2
  exit 2
fi
mapfile -t targets < <(sed 's/#.*//' "$project/targets" | awk 'NF { print $1 }')
if [ "${#targets[@]}" -eq 0 ]; then
  echo "$name: bazel/$name/targets lists no target" >&2
  exit 2
fi
integration=$(sed 's/#.*//' "$project/integration" | awk 'NF { print $1 }')
case $integration in
  /*) ;;
  *) integration=$repo_root/$integration ;;
esac
shopt -s nullglob
patches=("$integration"/*.patch)
shopt -u nullglob
if [ "${#patches[@]}" -eq 0 ]; then
  echo "$name: no krusty integration patch in $integration" >&2
  exit 2
fi
krusty=$(cd "$(dirname "$krusty")" && pwd)/$(basename "$krusty")

checkout=$work/src/$name
logs=$work/logs
mkdir -p "$logs"
"$here/checkout.sh" "$name" "$checkout"
if [ -f "$project/checkouts" ]; then
  while IFS=$'\t' read -r dir url commit; do
    git init -q "$checkout/$dir"
    git -C "$checkout/$dir" remote add origin "$url"
    git -C "$checkout/$dir" fetch -q --depth 1 origin "$commit"
    git -C "$checkout/$dir" checkout -q FETCH_HEAD
    if [ "$(git -C "$checkout/$dir" rev-parse HEAD)" != "$commit" ]; then
      echo "$name: $dir is not at the pinned $commit" >&2
      exit 1
    fi
  done < <(sed 's/#.*//' "$project/checkouts" | awk NF)
fi
for patch in "${patches[@]}"; do
  git -C "$checkout" apply "$patch"
done
export LANG=C.UTF-8 LC_ALL=C.UTF-8
# The integration's repository rule reads KRUSTY_BINARY from the whole client environment, not only
# from `--repo_env`, so a caller's own setting would route the reference build to krusty too. Only
# the krusty build names the binary.
unset KRUSTY_BINARY

# Bazel's output root lives in the work directory, beside the checkout. The project caps its server
# at 12 GB, more than a runner has beside the compilers.
startup=(--output_user_root="$work/bazel-root" --host_jvm_args=-Xmx6g)
common=(--disk_cache= --color=no --curses=no --show_progress_rate_limit=60)

bazel() {
  (cd "$checkout" && bash ./bazel.cmd "${startup[@]}" "$@")
}

# One measured build of the target set. `measure.py` records its cost: the Bazel server the client
# starts and its workers are descendants of it, and are stopped when the build ends. The exit status
# is left in `status`.
build() {
  local mode=$1
  shift
  local start code=0
  # The server that cleans is stopped, so the measured build starts one of its own under measure.py.
  bazel clean >>"$logs/$name.$mode.log" 2>&1
  bazel shutdown >>"$logs/$name.$mode.log" 2>&1
  start=$(date +%s)
  (
    cd "$checkout"
    python3 "$here/measure.py" "$logs/$name.$mode.metrics.json" \
      timeout 7200 bash ./bazel.cmd "${startup[@]}" build "${common[@]}" "$@" \
      --keep_going --repository_disable_download \
      --execution_log_json_file="$logs/$name.$mode.exec.json" --execution_log_sort=false \
      -- "${targets[@]}"
  ) >>"$logs/$name.$mode.log" 2>&1 || code=$?
  echo "$name $mode exit $code seconds $(($(date +%s) - start))" >>"$logs/STATUS"
  status=$code
  python3 "$here/bazel_compare.py" snapshot "$logs/$name.$mode.exec.json" "$execroot" \
    "$logs/$name.$mode.compilations.tsv" "$work/out/$name/$mode"
  rm -f "$logs/$name.$mode.exec.json"
}

rm -f "$logs/$name".*.log
bazel build "${common[@]}" --nobuild -- "${targets[@]}" >"$logs/$name.warmup.log" 2>&1
execroot=$(bazel info "${common[@]}" execution_root 2>>"$logs/$name.warmup.log")
build kotlinc
reference=$status
build krusty --repo_env=KRUSTY_BINARY="$krusty"
krusty_exit=$status
bazel shutdown >/dev/null 2>&1 || true
python3 "$here/bazel_compare.py" compare "$work" "$name" "$reference" "$krusty_exit"
# As for a Gradle project, a failed reference build is still compared as far as it got, then fails
# the run: every project is meant to build with kotlinc. krusty's own misses never fail it.
if [ "$reference" -ne 0 ]; then
  echo "$name: the reference kotlinc build failed (exit $reference); see $name.kotlinc.log" >&2
  exit 1
fi
