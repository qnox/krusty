#!/usr/bin/env bash
# Build one Cargo test target and print only its executable path. Cargo's JSON stream is also the
# sole carrier of rendered rustc diagnostics under `--message-format=json`; mirror those messages
# to stderr before selecting the artifact so command substitution in CI cannot swallow failures.
# With --build the selection goes to `cargo build`, which also builds the selected binaries.
# CARGO_TEST_BINARY_REUSE names library targets (e.g. `krusty krusty_native`) that an earlier
# build in this target directory already compiled with the same features; the build fails when
# Cargo compiles any of them again instead of reusing it.

set -euo pipefail

command=(test --no-run)
if [ "${1:-}" = --build ]; then
  command=(build)
  shift
fi
if [ "$#" -lt 2 ]; then
  echo "usage: $0 [--build] <cargo-target-name> <cargo target arguments...>" >&2
  exit 2
fi

target_name="$1"
shift

messages=$(mktemp)
trap 'rm -f "$messages"' EXIT
cargo "${command[@]}" --profile gate "$@" --message-format=json \
  | tee >(jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' >&2) \
  > "$messages"

bin=$(
  jq -r --arg target "$target_name" \
      'select(.reason == "compiler-artifact" and .target.name == $target and .executable != null) | .executable' \
      "$messages" \
    | tail -1
)

if [ -n "${CARGO_TEST_BINARY_REUSE:-}" ]; then
  rebuilt=$(
    jq -r --arg reuse "$CARGO_TEST_BINARY_REUSE" \
        'select(.reason == "compiler-artifact" and .fresh == false
                and (.target.kind | index("lib"))
                and (.target.name as $name | $reuse | split(" ") | index($name)))
         | .target.name' \
        "$messages" | sort -u | tr '\n' ' '
  )
  if [ -n "$rebuilt" ]; then
    echo "error: this build compiled ${rebuilt% } again instead of reusing the earlier build's" \
      "library. Its dependency features differ; align the dev-dependencies (see" \
      "crates/krusty-build/Cargo.toml) and compare with \`cargo ... --unit-graph\`." >&2
    exit 1
  fi
fi

if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  echo "could not locate $target_name test binary" >&2
  exit 1
fi

printf '%s\n' "$bin"
