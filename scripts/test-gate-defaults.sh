#!/usr/bin/env bash
# Canonical plain-gate process deadlines.
# Ordinary test processes share a two-minute ceiling. The product e2e suite is one process under
# its own deadline; do not split it into shards to stay under the ordinary ceiling.

export KRUSTY_TEST_TIMEOUT_SECONDS="${KRUSTY_TEST_TIMEOUT_SECONDS:-120}"
export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS:-120}"
export KRUSTY_E2E_TIMEOUT_SECONDS="${KRUSTY_E2E_TIMEOUT_SECONDS:-1800}"
export KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS:-600}"
export KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS:-600}"
# The reference kotlinc runs once per CLI corpus invocation, a JVM start each, in
# `conformance-regressions.sh`; it is one process with its own deadline.
export KRUSTY_CLI_REFERENCE_TIMEOUT_SECONDS="${KRUSTY_CLI_REFERENCE_TIMEOUT_SECONDS:-1800}"
# The diagnostics test data lane compiles every case with krusty and, on a cold verdict cache, with
# kotlinc; `diagnostics-conformance.sh` runs it as one process with its own deadline.
export KRUSTY_DIAGNOSTICS_TIMEOUT_SECONDS="${KRUSTY_DIAGNOSTICS_TIMEOUT_SECONDS:-10800}"
