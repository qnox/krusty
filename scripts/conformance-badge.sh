#!/usr/bin/env bash
# Turn one target report into its shields.io badge. `conformance` and `native-conformance` are case
# reports "<pct> <passed> <applicable>"; `jvm-byte-equality` is "<pct> <matched> <total>" class-file
# bytes (see conformance-report.sh). `just conformance-badge` writes local endpoint JSON; the
# master-only release job publishes the maximum version's reports.
#
#   scripts/conformance-badge.sh fields <kind> <report> <kotlin-version>   key=value lines ($GITHUB_OUTPUT)
#   scripts/conformance-badge.sh json <kind> <report> <kotlin-version>     shields.io endpoint JSON
set -euo pipefail

usage() {
  sed -n '2,9p' "$0" >&2
  exit 2
}
[ "$#" -eq 4 ] || usage
mode="$1"
kind="$2"
report="$3"
version="$4"
case "$mode" in
  fields | json) ;;
  *) usage ;;
esac
case "$kind" in
  conformance)
    name="case report"
    count_key=passed
    of_key=applicable
    unit=""
    noun=cases
    label="Kotlin $version conformance"
    ;;
  native-conformance)
    name="Native case report"
    count_key=passed
    of_key=applicable
    unit=""
    noun=cases
    label="Kotlin $version Native conformance"
    ;;
  jvm-byte-equality)
    name="JVM byte report"
    count_key=matched
    of_key=total
    unit=" bytes"
    noun=bytes
    label="Kotlin $version JVM byte equality"
    ;;
  *) usage ;;
esac
[[ $version =~ ^[0-9A-Za-z.-]+$ ]] || {
  echo "conformance-badge: invalid Kotlin version: $version" >&2
  exit 2
}

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/conformance-report.sh"

parsed="$(conformance_report_parse "$report")" || {
  echo "conformance-badge: invalid $name (want \"<pct> <$count_key> <$of_key>\" with $count_key <= $of_key): $report" >&2
  exit 1
}
read -r pct count of <<<"$parsed"
# The combined report's percentage is derived from its counts; one that disagrees was not produced
# by conformance-run.sh and must not reach the badge.
if [ "$(conformance_report_line "$count" "$of")" != "$parsed" ]; then
  echo "conformance-badge: percentage $pct% does not match $count/$of $noun: $report" >&2
  exit 1
fi

color=red
awk -v pct="$pct" 'BEGIN { exit !(pct >= 10) }' && color=orange
awk -v pct="$pct" 'BEGIN { exit !(pct >= 50) }' && color=yellow
awk -v pct="$pct" 'BEGIN { exit !(pct >= 70) }' && color=brightgreen
message="$pct% ($count/$of$unit)"

if [ "$mode" = fields ]; then
  printf 'pct=%s\n%s=%s\n%s=%s\nlabel=%s\nmessage=%s\ncolor=%s\n' \
    "$pct" "$count_key" "$count" "$of_key" "$of" "$label" "$message" "$color"
else
  printf '{"schemaVersion":1,"label":"%s","message":"%s","color":"%s"}\n' \
    "$label" "$message" "$color"
fi
