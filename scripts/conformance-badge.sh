#!/usr/bin/env bash
# Turn one combined box byte-equality report (the stdout of conformance-run.sh) into the conformance
# badge. `just conformance-badge` writes the local shields.io endpoint JSON; the master-only release
# job publishes the same fields from the maximum version's `pct-<version>` artifact.
#
#   scripts/conformance-badge.sh fields <report> <kotlin-version>   key=value lines ($GITHUB_OUTPUT)
#   scripts/conformance-badge.sh json <report> <kotlin-version>     shields.io endpoint JSON
set -euo pipefail

if [ "$#" -ne 3 ] || { [ "$1" != fields ] && [ "$1" != json ]; }; then
  sed -n '2,7p' "$0" >&2
  exit 2
fi
mode="$1"
report="$2"
version="$3"
[[ $version =~ ^[0-9A-Za-z.-]+$ ]] || {
  echo "conformance-badge: invalid Kotlin version: $version" >&2
  exit 2
}

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/conformance-report.sh"

parsed="$(conformance_report_parse "$report")" || {
  echo "conformance-badge: invalid byte report (want \"<pct> <matched> <total>\" with matched <= total): $report" >&2
  exit 1
}
read -r pct matched total <<<"$parsed"
# The combined report's percentage is derived from its counts; one that disagrees was not produced
# by conformance-run.sh and must not reach the badge.
if [ "$(conformance_report_line "$matched" "$total")" != "$parsed" ]; then
  echo "conformance-badge: percentage $pct% does not match $matched/$total bytes: $report" >&2
  exit 1
fi

color=red
awk -v pct="$pct" 'BEGIN { exit !(pct >= 10) }' && color=orange
awk -v pct="$pct" 'BEGIN { exit !(pct >= 50) }' && color=yellow
awk -v pct="$pct" 'BEGIN { exit !(pct >= 70) }' && color=brightgreen
label="Kotlin $version byte conformance"
message="$pct% ($matched/$total bytes)"

if [ "$mode" = fields ]; then
  printf 'pct=%s\nmatched=%s\ntotal=%s\nlabel=%s\nmessage=%s\ncolor=%s\n' \
    "$pct" "$matched" "$total" "$label" "$message" "$color"
else
  printf '{"schemaVersion":1,"label":"%s","message":"%s","color":"%s"}\n' \
    "$label" "$message" "$color"
fi
