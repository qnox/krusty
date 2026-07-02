#!/usr/bin/env bash
# Re-measure coverage and write the result to coverage-baseline.json.
#
# Run this only when a coverage change is intentional (new tests raised it, or a justified drop was
# reviewed). The baseline is what the pre-push gate compares against, so committing it is committing
# to "no future regression below this line". Commit coverage-baseline.json alongside the change.
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/coverage.sh coverage-baseline.json
echo "coverage-bless: wrote coverage-baseline.json" >&2
