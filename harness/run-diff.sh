#!/usr/bin/env bash
# Differential test harness: krusty vs the real kotlinc over the box corpus (ABI signatures +
# execution). The reference toolchain (kotlinc) and the box corpus are SELF-PROVISIONED and cached by
# `just` — the version is pinned by the `kotlin-versions` manifest (currently 2.4.0). No manual dist,
# no `$KRUSTY_KOTLINC`, no JDK path to set: just run this. Honors ambient overrides if present.
set -euo pipefail
cd "$(dirname "$0")/.."
exec just conformance
