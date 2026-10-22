#!/usr/bin/env bash
# Hands Bazel the binary `cargo build --release` produced.
#
# The path is absolute rather than $HOME-relative on purpose: Bazel's sandbox does not pass HOME,
# and `set -u` makes an unset one a fatal "HOME: unbound variable" inside the action.
set -euo pipefail
krusty="${KRUSTY_BINARY:?set KRUSTY_BINARY to the krusty binary built by cargo}"
exec "$krusty" "$@"
