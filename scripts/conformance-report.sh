#!/usr/bin/env bash
# The two box conformance reports shared by the box test, `conformance-run.sh`, and the badges. Both
# are one line, a one-decimal percentage of two integer counts over the same applicable box cases:
#
#   <pct> <passed> <applicable>   case report (KRUSTY_CONFORMANCE_REPORT, conformance badge)
#   <pct> <matched> <total>       JVM byte report (KRUSTY_JVM_BYTE_REPORT, JVM byte-equality badge)
#
# `passed` is the applicable cases whose box() returned "OK". `matched` and `total` are `.class`
# byte counts: matching leading bytes per (module, class) pair against the longer of the two
# lengths, a missing or extra class at its full length, and a failed box at zero against its
# reference classes. `pct` is the one-decimal display of 100 * count / of, and 0.0 when of is 0.
# Neither report carries the other's counts; the outcome manifests gate box() results separately.

# Validate the single report line in file $1 and print it as "<pct> <count> <of>". Return 1 on a
# missing, unreadable, or malformed report, or one whose count exceeds its denominator. Counts carry
# no leading zeros (Bash arithmetic would read them as octal) and at most 15 digits, so summing
# every shard stays within 64-bit arithmetic.
conformance_report_parse() {
  local content pct count of
  [ -f "$1" ] && [ -r "$1" ] || return 1
  content="$(<"$1")"
  [[ $content =~ ^(0|[1-9][0-9]{0,2})\.[0-9]\ (0|[1-9][0-9]{0,14})\ (0|[1-9][0-9]{0,14})$ ]] ||
    return 1
  read -r pct count of <<<"$content"
  [ "$count" -le "$of" ] || return 1
  printf '%s %s %s\n' "$pct" "$count" "$of"
}

# Print the report line for integer counts $1 (count) and $2 (of).
conformance_report_line() {
  local pct
  pct="$(awk -v count="$1" -v of="$2" \
    'BEGIN { printf "%.1f", of == 0 ? 0 : 100 * count / of }')"
  printf '%s %s %s\n' "$pct" "$1" "$2"
}
