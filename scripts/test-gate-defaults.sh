#!/usr/bin/env bash
# Canonical plain-gate process deadlines.
# Ordinary test processes share a two-minute ceiling. The product e2e suite is one process under
# its own deadline; do not split it into shards to stay under the ordinary ceiling.

export KRUSTY_TEST_TIMEOUT_SECONDS="${KRUSTY_TEST_TIMEOUT_SECONDS:-120}"
export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS:-120}"
export KRUSTY_E2E_TIMEOUT_SECONDS="${KRUSTY_E2E_TIMEOUT_SECONDS:-1800}"
export KRUSTY_CONFORMANCE_SHARDS="${KRUSTY_CONFORMANCE_SHARDS:-4}"
