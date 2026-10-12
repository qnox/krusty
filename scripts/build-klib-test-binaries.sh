#!/usr/bin/env bash
# Build the focused KLIB e2e and library-unit binaries in one Cargo invocation. They are one
# artifact family consumed by the maximum-version Native conformance row.

set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <output-directory>" >&2
  exit 2
fi

output_dir="$1"
[ -d "$output_dir" ] || { echo "output directory does not exist: $output_dir" >&2; exit 1; }

artifacts=$(mktemp)
trap 'rm -f "$artifacts"' EXIT

set +e
cargo test --no-run --profile gate -p krusty --lib --test e2e \
  --message-format=json >"$artifacts"
cargo_status=$?
jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' \
  "$artifacts" >&2
jq_status=$?
set -e

if [ "$cargo_status" -ne 0 ]; then
  exit "$cargo_status"
fi
if [ "$jq_status" -ne 0 ]; then
  exit "$jq_status"
fi

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
    echo "could not locate $kind target $target in the shared Cargo build" >&2
    exit 1
  fi
  cp "$executable" "$output_dir/$destination"
}

copy_artifact e2e test klib-e2e-tests
copy_artifact krusty lib klib-unit-tests
