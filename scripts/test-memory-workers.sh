#!/usr/bin/env bash
# Exercise scripts/memory-workers.sh against stand-in /proc trees: the worker count is the CPU
# count capped by memory at the given per-worker footprint, and never below one.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

# machine <name> <cpus> <MemTotal kB or -> creates a stand-in /proc under the fixture.
machine() {
    local root=$fixture/$1 i
    mkdir -p "$root/proc"
    : > "$root/proc/cpuinfo"
    for ((i = 0; i < $2; i++)); do
        printf 'processor\t: %d\nmodel name\t: Stand-in CPU\n\n' "$i" >> "$root/proc/cpuinfo"
    done
    if [[ $3 == - ]]; then
        rm -f "$root/proc/meminfo"
    else
        printf 'MemTotal:       %s kB\nMemFree:         1048576 kB\n' "$3" > "$root/proc/meminfo"
    fi
}

failures=0
expect() {
    local name=$1 per_worker=$2 expected=$3 actual
    actual=$(MEMORY_WORKERS_ROOT=$fixture/$name bash "$repo/scripts/memory-workers.sh" "$per_worker")
    if [[ $actual != "$expected" ]]; then
        echo "FAIL $name ($per_worker MiB a worker): expected $expected, got $actual" >&2
        failures=$((failures + 1))
    fi
}

# The self-hosted 4-CPU runner reports 7.6 GiB; GitHub's 4-CPU runner 15.6 GiB.
machine small 4 7969177
machine github 4 16357892
machine tiny 4 1048576
machine wide 32 16357892
machine unknown 6 -

expect small 2048 3
expect github 2048 4
expect tiny 2048 1
expect wide 2048 7
expect unknown 2048 6

for bad in "" 0 -1 two; do
    if MEMORY_WORKERS_ROOT=$fixture/small bash "$repo/scripts/memory-workers.sh" $bad >/dev/null 2>&1; then
        echo "FAIL a per-worker footprint of '$bad' is refused" >&2
        failures=$((failures + 1))
    fi
done

if [[ $failures -ne 0 ]]; then
    exit 1
fi
echo "memory-workers: all cases pass"
