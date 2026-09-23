#!/usr/bin/env bash
# Canonical plain-gate process deadlines and partitioning. Every ordinary test process receives the
# same two-minute ceiling; workloads that exceed it must be partitioned rather than granted an
# exception here.

export KRUSTY_TEST_TIMEOUT_SECONDS="${KRUSTY_TEST_TIMEOUT_SECONDS:-120}"
export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS:-120}"
export KRUSTY_E2E_TIMEOUT_SECONDS="${KRUSTY_E2E_TIMEOUT_SECONDS:-120}"
export KRUSTY_CONFORMANCE_SHARDS="${KRUSTY_CONFORMANCE_SHARDS:-4}"
export KRUSTY_NATIVE_CONFORMANCE_SHARDS="${KRUSTY_NATIVE_CONFORMANCE_SHARDS:-4}"
export KRUSTY_E2E_SHARDS="${KRUSTY_E2E_SHARDS:-22}"
