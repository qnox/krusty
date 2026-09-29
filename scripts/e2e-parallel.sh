#!/usr/bin/env bash
# How many e2e shards the plain gate runs at once.
#
# Each shard already threads across the host (`--test-threads` = CPU count), but those threads spend
# most of their time waiting on box-runner JVMs, so one shard does not fill the machine. A few shards
# at once recover that idle time. Past three, a 4-core host stops gaining throughput and each shard
# slows toward its deadline, so the default stays at three whenever the host has at least three CPUs
# and at one otherwise. An explicit KRUSTY_E2E_PARALLEL overrides the default and is still capped by
# the shard count.

e2e_parallel_width() {
  local ncpu="$1" shards="$2" override="${3:-}" width
  if [ -n "$override" ]; then
    case "$override" in
      '' | *[!0-9]* | 0*)
        return 2
        ;;
    esac
    width="$override"
  elif [ "$ncpu" -ge 3 ]; then
    width=3
  else
    width=1
  fi
  if [ "$width" -gt "$shards" ]; then
    width="$shards"
  fi
  printf '%s\n' "$width"
}
