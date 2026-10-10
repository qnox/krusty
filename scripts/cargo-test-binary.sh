#!/usr/bin/env bash
# Build one Cargo test target and print only its executable path. Cargo's JSON stream is also the
# sole carrier of rendered rustc diagnostics under `--message-format=json`; mirror those messages
# to stderr before selecting the artifact so command substitution in CI cannot swallow failures.

set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <cargo-target-name> <cargo test target arguments...>" >&2
  exit 2
fi

target_name="$1"
shift

bin=$(
  cargo test --no-run --profile gate "$@" --message-format=json \
    | tee >(jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' >&2) \
    | jq -r --arg target "$target_name" \
        'select(.reason == "compiler-artifact" and .target.name == $target and .executable != null) | .executable' \
    | tail -1
)

if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  echo "could not locate $target_name test binary" >&2
  exit 1
fi

printf '%s\n' "$bin"
