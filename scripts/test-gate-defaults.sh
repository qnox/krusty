#!/usr/bin/env bash
# Canonical plain-gate process deadlines and partitioning. Every ordinary test process receives the
# same two-minute ceiling; workloads that exceed it must be partitioned rather than granted an
# exception here.

export KRUSTY_TEST_TIMEOUT_SECONDS="${KRUSTY_TEST_TIMEOUT_SECONDS:-120}"
export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS:-120}"
export KRUSTY_E2E_TIMEOUT_SECONDS="${KRUSTY_E2E_TIMEOUT_SECONDS:-120}"
export KRUSTY_CONFORMANCE_SHARDS="${KRUSTY_CONFORMANCE_SHARDS:-4}"
# The scored conformance run (`conformance-run.sh`) reference-compiles every applicable box case
# with the real kotlinc, roughly an order of magnitude more work per case than the plain gate's
# box pass. It partitions the corpus more finely than the plain gate, but reference-compiling even
# one twelfth of the corpus cold (the ref-class cache is empty on a first run or a CI cache miss)
# plus the JVM/kotlinc-server teardown still exceeds the plain gate's unit-test ceiling, so this
# workload gets its own larger per-shard deadline rather than being sharded into a startup-bound
# corner. The ceiling never slows a warm run: a cached shard finishes in seconds, far inside it.
export KRUSTY_SCORED_CONFORMANCE_SHARDS="${KRUSTY_SCORED_CONFORMANCE_SHARDS:-12}"
export KRUSTY_SCORED_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_SCORED_CONFORMANCE_TIMEOUT_SECONDS:-300}"
export KRUSTY_E2E_SHARDS="${KRUSTY_E2E_SHARDS:-22}"
