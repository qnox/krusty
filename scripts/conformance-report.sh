#!/usr/bin/env bash
# The box byte-equality report shared by the box test, `conformance-run.sh`, and the badge:
#
#   <pct> <matched> <total>
#
# `matched` and `total` are integer `.class` byte counts summed over the applicable box cases:
# matching leading bytes per (module, class) pair against the longer of the two lengths, a missing
# or extra class at its full length, and a failed box at zero against its reference classes. `pct`
# is the one-decimal display of 100 * matched / total, and 0.0 when total is 0. Box pass/fail counts
# are never part of this line; the outcome manifests gate them separately.

# Validate the single report line in file $1 and print it as "<pct> <matched> <total>". Return 1 on
# a missing, unreadable, or malformed report, or one with matched > total. Counts carry no leading
# zeros (Bash arithmetic would read them as octal) and at most 15 digits, so summing every shard
# stays within 64-bit arithmetic.
conformance_report_parse() {
  local content pct matched total
  [ -f "$1" ] && [ -r "$1" ] || return 1
  content="$(<"$1")"
  [[ $content =~ ^(0|[1-9][0-9]{0,2})\.[0-9]\ (0|[1-9][0-9]{0,14})\ (0|[1-9][0-9]{0,14})$ ]] ||
    return 1
  read -r pct matched total <<<"$content"
  [ "$matched" -le "$total" ] || return 1
  printf '%s %s %s\n' "$pct" "$matched" "$total"
}

# Print the report line for integer byte counts $1 (matched) and $2 (total).
conformance_report_line() {
  local pct
  pct="$(awk -v matched="$1" -v total="$2" \
    'BEGIN { printf "%.1f", total == 0 ? 0 : 100 * matched / total }')"
  printf '%s %s %s\n' "$pct" "$1" "$2"
}
