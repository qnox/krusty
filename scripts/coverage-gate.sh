#!/usr/bin/env bash
# Enforce that coverage does not regress below the committed master baseline.
#
# Runs the full instrumented suite (scripts/coverage.sh) and compares every metric — regions,
# functions, lines, branches — against coverage-baseline.json. Any metric more than EPS percentage
# points below baseline fails the gate. EPS absorbs the small run-to-run jitter LLVM coverage has
# (thread scheduling changes which branches a few racy tests happen to hit); it is not a licence to
# drop coverage.
#
# Refresh the baseline deliberately after a real coverage change with: scripts/coverage-bless.sh
set -euo pipefail

cd "$(dirname "$0")/.."
baseline="coverage-baseline.json"
current="target/coverage/summary.json"
EPS="${KRUSTY_COVERAGE_EPS:-0.10}"

if [ ! -f "$baseline" ]; then
  echo "coverage-gate: no $baseline — run scripts/coverage-bless.sh to establish one" >&2
  exit 2
fi

scripts/coverage.sh "$current"

python3 - "$baseline" "$current" "$EPS" <<'PY'
import json, sys
baseline, current, eps = sys.argv[1], sys.argv[2], float(sys.argv[3])
b = json.load(open(baseline))
c = json.load(open(current))
fail = False
print("coverage-gate: metric        baseline   current    delta")
for k in ("regions", "functions", "lines", "branches"):
    bp = b[k]["percent"]
    cp = c[k]["percent"]
    d = cp - bp
    mark = "OK"
    if d < -eps:
        mark = "REGRESSED"
        fail = True
    print(f"  {k:10s}  {bp:8.2f}%  {cp:8.2f}%  {d:+7.2f}pp  {mark}")
if fail:
    print(f"coverage-gate: FAIL — a metric dropped more than {eps}pp below master baseline", file=sys.stderr)
    sys.exit(1)
print("coverage-gate: PASS — coverage >= master baseline")
PY
