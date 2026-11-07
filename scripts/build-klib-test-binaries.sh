#!/usr/bin/env bash
# Build the focused KLIB e2e and library-unit binaries. They are one artifact family consumed by
# the maximum-version Native conformance row. The two targets compile the whole `krusty` crate
# twice (once as the library behind klib_e2e, once with cfg(test)); separate Cargo invocations keep
# those two compiles from running at the same time and adding up their peak memory.
#
# `KRUSTY_KLIB_CRATE_OPT_LEVEL`, when set to 0, 1, 2, 3, s, or z, overrides only the `krusty`
# package opt-level. The 8 GB CI pool sets 0: opt-level 1 of the cfg(test) crate is killed there.

set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <output-directory>" >&2
  exit 2
fi

output_dir="$1"
[ -d "$output_dir" ] || { echo "output directory does not exist: $output_dir" >&2; exit 1; }

artifacts=$(mktemp)
trap 'rm -f "$artifacts"' EXIT

cargo_config=()
if [ -n "${KRUSTY_KLIB_CRATE_OPT_LEVEL:-}" ]; then
  case "$KRUSTY_KLIB_CRATE_OPT_LEVEL" in
    0|1|2|3|s|z) ;;
    *)
      echo "KRUSTY_KLIB_CRATE_OPT_LEVEL must be 0, 1, 2, 3, s, or z" >&2
      exit 2
      ;;
  esac
  cargo_config+=(--config "profile.klib-test.package.krusty.opt-level=${KRUSTY_KLIB_CRATE_OPT_LEVEL}")
fi

build() {
  local step cargo_status jq_status
  step=$(mktemp)
  set +e
  cargo test --no-run --profile klib-test -p krusty "${cargo_config[@]}" "$@" \
    --features focused-klib-tests \
    --message-format=json >"$step"
  cargo_status=$?
  jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' \
    "$step" >&2
  jq_status=$?
  set -e
  cat "$step" >>"$artifacts"
  rm -f "$step"

  if [ "$cargo_status" -ne 0 ]; then
    exit "$cargo_status"
  fi
  if [ "$jq_status" -ne 0 ]; then
    exit "$jq_status"
  fi
}

build --test klib_e2e
build --lib

copy_artifact() {
  local target="$1" kind="$2" destination="$3" executable
  executable=$(
    jq -r --arg target "$target" --arg kind "$kind" \
      'select(.reason == "compiler-artifact"
              and .target.name == $target
              and (.target.kind | index($kind)) != null
              and .executable != null) | .executable' \
      "$artifacts" | tail -1
  )
  if [ -z "$executable" ] || [ ! -x "$executable" ]; then
    echo "could not locate $kind target $target in the Cargo build output" >&2
    exit 1
  fi
  cp "$executable" "$output_dir/$destination"
}

copy_artifact klib_e2e test klib-e2e-tests
copy_artifact krusty lib klib-unit-tests
