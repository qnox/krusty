#!/usr/bin/env bash
# Wall-clock durations for a script's phases.
#
# A caller sets phase_log_prefix (the word before ": phase") and may set PHASE_TIMING_LOG to a
# file. phase_begin prints when a phase starts. phase_end prints its whole-second duration and, when
# the log is set, appends "name<TAB>seconds" so a child process can record into the same ledger.
# phase_report repeats every finished phase and a total. total is wall time since the first
# phase_begin in this process, not the sum of the rows: parallel phases overlap.

phase_log_prefix="${phase_log_prefix:-phase}"

phase_begin() {
  local name="$1"
  declare -gA phase_started
  if [ -z "${phase_origin+x}" ]; then
    phase_origin=$SECONDS
  fi
  phase_started["$name"]=$SECONDS
  echo "${phase_log_prefix}: phase start ${name}" >&2
}

phase_end() {
  local name="$1"
  declare -gA phase_started
  local started="${phase_started[$name]:-$SECONDS}"
  local elapsed=$((SECONDS - started))
  echo "${phase_log_prefix}: phase ${name} ${elapsed}s" >&2
  if [ -n "${PHASE_TIMING_LOG:-}" ]; then
    printf '%s\t%s\n' "$name" "$elapsed" >>"$PHASE_TIMING_LOG"
  fi
}

phase_report() {
  local total=$((SECONDS - ${phase_origin:-0}))
  echo "${phase_log_prefix}: phases" >&2
  if [ -n "${PHASE_TIMING_LOG:-}" ] && [ -f "$PHASE_TIMING_LOG" ]; then
    local name elapsed
    while IFS=$'\t' read -r name elapsed; do
      [ -n "$name" ] || continue
      echo "${phase_log_prefix}: phase ${name} ${elapsed}s" >&2
    done <"$PHASE_TIMING_LOG"
  fi
  echo "${phase_log_prefix}: phase total ${total}s" >&2
}
