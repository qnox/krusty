#!/usr/bin/env bash
# Print how many test workers this host can run side by side: one per CPU, but no more than its
# memory holds at the given peak footprint per worker, and at least one.
#
# Usage: memory-workers.sh <MiB per worker>
#
# A worker's footprint is measured, not guessed: the product e2e suite peaked at 6.5 GB above an
# idle host with four test threads (about 1.6 GiB a thread: the test process's in-process compiles,
# its compiler-server JVMs, and a `krusty kotlinc-native` child per thread), so its callers pass
# 2048. Without a memory cap a 4-CPU host with 8 GB and no swap runs four and is OOM-killed (exit
# 137) while a 16 GB host with the same CPUs passes.
#
# MEMORY_WORKERS_ROOT replaces / for /proc reads, so the tests can stand in a machine.
set -euo pipefail

if [ "$#" -ne 1 ] || ! [[ $1 =~ ^[1-9][0-9]*$ ]]; then
  echo "usage: $0 <MiB per worker>" >&2
  exit 2
fi
per_worker_mib=$1
root=${MEMORY_WORKERS_ROOT:-}

cpus=$(grep -c '^processor' "$root/proc/cpuinfo" 2>/dev/null || true)
if [ -z "$root" ]; then
  # The scheduler's view (affinity and cgroup cpusets), as the test binaries' own default is.
  cpus=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo "${cpus:-1}")
fi
[ "${cpus:-0}" -ge 1 ] || cpus=1

mem_kib=$(awk '$1 == "MemTotal:" { print $2; exit }' "$root/proc/meminfo" 2>/dev/null || true)
if [ -z "$mem_kib" ]; then
  # No memory figure (not Linux): the CPU count alone, as before.
  echo "$cpus"
  exit 0
fi
by_memory=$((mem_kib / 1024 / per_worker_mib))
workers=$((cpus < by_memory ? cpus : by_memory))
[ "$workers" -ge 1 ] || workers=1
echo "$workers"
