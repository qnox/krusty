#!/usr/bin/env bash
# Canonical plain-gate process deadlines and partitioning. Keep every timeout below five minutes.

export KRUSTY_TEST_TIMEOUT_SECONDS="${KRUSTY_TEST_TIMEOUT_SECONDS:-120}"
export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS:-295}"
export KRUSTY_E2E_TIMEOUT_SECONDS="${KRUSTY_E2E_TIMEOUT_SECONDS:-295}"
export KRUSTY_E2E_SHARDS="${KRUSTY_E2E_SHARDS:-22}"
