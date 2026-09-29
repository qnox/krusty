#!/usr/bin/env bash
# How the plain gate schedules the e2e shards.
#
# One shard does not fill a 4-core host: its threads wait on box-runner JVMs, and even a CPU-heavy
# shard uses about two cores. Running the twenty-two shards one at a time therefore leaves the
# machine idle. Three shards at `--test-threads` equal to the CPU count is the other failure: a
# shard that takes about 40 seconds alone ran past the two-minute deadline once two neighbors were
# in flight. Three shards with half as many threads each stayed inside that deadline and finished
# the pass sooner than either extreme.
#
# Defaults, before an explicit override:
#   * 4 or more CPUs: 3 shards at a time, each with ncpu/2 threads
#   * fewer CPUs: 1 shard at a time, using every CPU
# KRUSTY_E2E_PARALLEL and KRUSTY_E2E_THREADS override the two numbers. The width never exceeds the
# shard count. An override that is not a canonical positive integer is rejected.

e2e_schedule() {
  local ncpu="$1" shards="$2" width_override="${3:-}" threads_override="${4:-}"
  local width threads
  if [ -n "$width_override" ]; then
    case "$width_override" in
      '' | *[!0-9]* | 0*)
        return 2
        ;;
    esac
    width="$width_override"
  elif [ "$ncpu" -ge 4 ]; then
    width=3
  else
    width=1
  fi
  if [ "$width" -gt "$shards" ]; then
    width="$shards"
  fi
  if [ -n "$threads_override" ]; then
    case "$threads_override" in
      '' | *[!0-9]* | 0*)
        return 2
        ;;
    esac
    threads="$threads_override"
  elif [ "$width" -gt 1 ]; then
    threads=$((ncpu / 2))
    if [ "$threads" -lt 1 ]; then
      threads=1
    fi
  else
    threads="$ncpu"
    if [ "$threads" -lt 1 ]; then
      threads=1
    fi
  fi
  printf '%s %s\n' "$width" "$threads"
}
