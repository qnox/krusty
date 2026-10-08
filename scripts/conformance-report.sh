#!/usr/bin/env bash
# The box conformance reports shared by target tests, runners, and badges. Each is one line with a
# one-decimal percentage of two integer counts:
#
#   <pct> <passed> <applicable>   case report (JVM or Native conformance badge)
#   <pct> <matched> <total>       JVM byte report (KRUSTY_JVM_BYTE_REPORT, JVM byte-equality badge)
#
# `passed` is the applicable cases whose box() returned "OK". `matched` and `total` are `.class`
# byte counts: a paired class contributes its entire length only when the complete class file is
# byte-identical; a differing, missing, or extra class contributes zero against the full/longer
# length, and a failed box contributes zero against its reference classes. `pct` is the one-decimal
# display of 100 * count / of, and 0.0 when of is 0. Neither report carries the other's counts; the
# outcome manifests gate box() results separately.

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

# Print the report line for integer counts $1 (count) and $2 (of). Round down to one decimal so a
# non-perfect ratio can never be displayed as 100.0%.
conformance_report_line() {
  local tenths=0
  if [ "$2" -ne 0 ]; then
    tenths=$((1 * "$1" * 1000 / "$2"))
  fi
  printf '%d.%d %s %s\n' "$((tenths / 10))" "$((tenths % 10))" "$1" "$2"
}
