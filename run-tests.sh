#!/usr/bin/env bash
# Canonical test runner for krusty. Use only this script to run the suite.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

cd "$(dirname "$0")"

# Default to the fast-iteration `gate` profile (unoptimized → seconds-long rebuilds, but with
# overflow-checks/assertions off so krusty's wrapping arithmetic doesn't abort). The in-loop tests don't
# need optimization; this keeps the round well under the 60s budget. Pass --release or --profile to override.
profile_arg="--profile gate"
for a in "$@"; do
  case "$a" in
    --release|--profile|--profile=*) profile_arg="" ;;
  esac
done

exec cargo test $profile_arg "$@"
