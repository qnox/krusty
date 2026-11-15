#!/usr/bin/env bash
# Exercise scripts/runner-profile.sh against stand-in /proc and /sys trees: the report names the
# machine and splits the CPU time, pressure stalls and throttling between start and report.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

# machine <root> <uptime> <cpu line> <cpu psi> <io psi> <memory psi> <throttled usec or ->
machine() {
    local root=$fixture/$1
    mkdir -p "$root/proc/pressure" "$root/sys/fs/cgroup"
    printf 'processor\t: 0\nmodel name\t: Stand-in CPU @ 2.00GHz\nprocessor\t: 1\nmodel name\t: Stand-in CPU @ 2.00GHz\n' \
        > "$root/proc/cpuinfo"
    printf 'MemTotal:        8388608 kB\nMemFree:         1048576 kB\nSwapTotal:       2097152 kB\nSwapFree:        1048576 kB\n' \
        > "$root/proc/meminfo"
    echo "$2.57 100.00" > "$root/proc/uptime"
    printf 'cpu  %s 0 0\ncpu0 1 2 3 4 5 6 7 8 0 0\n' "$3" > "$root/proc/stat"
    local resource total
    for resource in cpu io memory; do
        case $resource in cpu) total=$4 ;; io) total=$5 ;; memory) total=$6 ;; esac
        if [[ $total == - ]]; then
            rm -f "$root/proc/pressure/$resource"
        else
            printf 'some avg10=0.00 avg60=0.00 avg300=0.00 total=%s\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=1\n' \
                "$total" > "$root/proc/pressure/$resource"
        fi
    done
    if [[ $7 == - ]]; then
        rm -rf "$root/sys/fs/cgroup"
    else
        echo '200000 100000' > "$root/sys/fs/cgroup/cpu.max"
        printf 'usage_usec 5\nnr_throttled 3\nthrottled_usec %s\n' "$7" > "$root/sys/fs/cgroup/cpu.stat"
    fi
}

failures=0
expect() {
    local name=$1 expected=$2
    if [[ $(cat "$fixture/$name.out") != "$expected" ]]; then
        echo "$name: expected" >&2
        echo "$expected" >&2
        echo "$name: got" >&2
        cat "$fixture/$name.out" >&2
        failures=$((failures + 1))
    fi
}

# user nice system idle iowait irq softirq steal: 1000 ticks pass, 400 of them stolen.
machine contended-start 1000 '100 0 50 800 0 0 0 0' 1000000 2000000 0 5000000
machine contended-end 1600 '400 0 150 900 100 0 0 400' 31000000 4000000 0 65000000
RUNNER_PROFILE_ROOT=$fixture/contended-start "$repo/scripts/runner-profile.sh" start "$fixture/contended.state"
RUNNER_PROFILE_ROOT=$fixture/contended-end "$repo/scripts/runner-profile.sh" report "$fixture/contended.state" \
    > "$fixture/contended.out"
expect contended "machine: 2 cpus (Stand-in CPU @ 2.00GHz), cpu quota 2.00 cpus, memory 8.0 GiB, swap 1.0 of 2.0 GiB used
over 600s: cpu user 30% system 10% iowait 10% steal 40% idle 10% | stalled on cpu 30s, io 2s, memory 0s | throttled 60s"

# A kernel without pressure stall information and a job without a cgroup v2 quota.
machine bare-start 10 '0 0 0 100 0 0 0 0' - - - -
machine bare-end 20 '50 0 0 150 0 0 0 0' - - - -
RUNNER_PROFILE_ROOT=$fixture/bare-start "$repo/scripts/runner-profile.sh" start "$fixture/bare.state"
RUNNER_PROFILE_ROOT=$fixture/bare-end "$repo/scripts/runner-profile.sh" report "$fixture/bare.state" > "$fixture/bare.out"
expect bare "machine: 2 cpus (Stand-in CPU @ 2.00GHz), cpu quota max, memory 8.0 GiB, swap 1.0 of 2.0 GiB used
over 10s: cpu user 50% system 0% iowait 0% steal 0% idle 50% | stalled on cpu n/a, io n/a, memory n/a | throttled n/a"

status=0
"$repo/scripts/runner-profile.sh" report "$fixture/missing.state" > "$fixture/missing.out" 2>&1 || status=$?
expect missing "no runner profile was started at $fixture/missing.state"
if [[ $status -ne 1 ]]; then
    echo "missing: expected status 1, got $status" >&2
    failures=$((failures + 1))
fi

if [[ $failures -ne 0 ]]; then
    echo "$failures runner profile case(s) failed" >&2
    exit 1
fi
echo "runner profile: 3 cases passed"
