#!/usr/bin/env bash
# Canonical plain-gate process deadlines. Small binaries keep a two-minute hang bound. The
# conformance corpus and the e2e binary each run as one process and use every CPU on the host, so
# their hang bounds cover that whole process. `KRUSTY_CONFORMANCE_SHARDS` remains an override for a
# host that cannot hold the corpus in one process; the plain gate does not partition.

export KRUSTY_TEST_TIMEOUT_SECONDS="${KRUSTY_TEST_TIMEOUT_SECONDS:-120}"
export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS="${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS:-900}"
export KRUSTY_E2E_TIMEOUT_SECONDS="${KRUSTY_E2E_TIMEOUT_SECONDS:-900}"
export KRUSTY_CONFORMANCE_SHARDS="${KRUSTY_CONFORMANCE_SHARDS:-1}"
