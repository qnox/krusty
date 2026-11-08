#!/usr/bin/env bash
# Package the validated max-version reports as immutable shields.io release assets.
#   scripts/package-release-badges.sh KOTLIN_VERSION REPORT_DIR OUTPUT_DIR
set -euo pipefail
if [[ $# -ne 3 ]]; then
  echo "usage: $0 KOTLIN_VERSION REPORT_DIR OUTPUT_DIR" >&2
  exit 2
fi
version=$1
reports=$2
output=$3
repo=$(cd "$(dirname "$0")/.." && pwd)
badge_stage=$(mktemp -d)
trap 'rm -rf "$badge_stage"' EXIT

# Validate every report before exposing any of the package to the release uploader.
bash "$repo/scripts/conformance-badge.sh" json conformance "$reports/pct.txt" "$version" \
  > "$badge_stage/krusty-conformance.json"
bash "$repo/scripts/conformance-badge.sh" json jvm-byte-equality "$reports/jvm-byte-equality.txt" "$version" \
  > "$badge_stage/krusty-jvm-byte-equality.json"
bash "$repo/scripts/conformance-badge.sh" json native-conformance "$reports/native-pct.txt" "$version" \
  > "$badge_stage/krusty-native-conformance.json"
jq -cn --arg v "$version" '{schemaVersion: 1, label: "Kotlin", message: $v, color: "blue"}' \
  > "$badge_stage/krusty-kotlin.json"
mkdir -p -- "$output"
cp -- "$badge_stage/"*.json "$output/"
