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

# The cases below read no kernel log unless they stand one in.
export RUNNER_PROFILE_DMESG=true

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

# A root cgroup: cpu.stat holds usage counters but no throttling counter.
machine root-start 10 '0 0 0 100 0 0 0 0' 0 0 0 5
machine root-end 20 '50 0 0 150 0 0 0 0' 0 0 0 5
for state in start end; do
    rm "$fixture/root-$state/sys/fs/cgroup/cpu.max"
    printf 'usage_usec 5\nuser_usec 3\nsystem_usec 2\n' > "$fixture/root-$state/sys/fs/cgroup/cpu.stat"
done
RUNNER_PROFILE_ROOT=$fixture/root-start "$repo/scripts/runner-profile.sh" start "$fixture/root.state"
RUNNER_PROFILE_ROOT=$fixture/root-end "$repo/scripts/runner-profile.sh" report "$fixture/root.state" > "$fixture/root.out"
expect root "machine: 2 cpus (Stand-in CPU @ 2.00GHz), cpu quota max, memory 8.0 GiB, swap 1.0 of 2.0 GiB used
over 10s: cpu user 50% system 0% iowait 0% steal 0% idle 50% | stalled on cpu 0s, io 0s, memory 0s | throttled n/a"

# A kernel that killed a process for memory: the report names it, ahead of the counters. The job
# started at uptime 10, so the kills at 3.4 and 9.9 belong to earlier jobs and are dropped; a kill
# at the start second is kept, and a line without an uptime timestamp is kept and marked undated.
cat > "$fixture/dmesg" <<'STUB'
#!/usr/bin/env bash
echo '[ 10.1] audit: something unrelated'
echo '[ 3.4] Out of memory: Killed process 7 (java) total-vm:1kB, anon-rss:1kB'
echo '[ 9.9] Out of memory: Killed process 8 (java) total-vm:1kB, anon-rss:1kB'
echo '[ 10.0] Out of memory: Killed process 9 (java) total-vm:2kB, anon-rss:2kB'
echo 'Mon Nov 25 07:00:00 2026 Out of memory: Killed process 10 (java) total-vm:3kB, anon-rss:3kB'
echo '[ 99.2] Out of memory: Killed process 4242 (java) total-vm:9000000kB, anon-rss:3100000kB'
STUB
chmod +x "$fixture/dmesg"
RUNNER_PROFILE_DMESG=$fixture/dmesg RUNNER_PROFILE_ROOT=$fixture/root-end \
    "$repo/scripts/runner-profile.sh" report "$fixture/root.state" > "$fixture/oom.out"
expect oom "machine: 2 cpus (Stand-in CPU @ 2.00GHz), cpu quota max, memory 8.0 GiB, swap 1.0 of 2.0 GiB used
memory kills recorded by the kernel:
[ 10.0] Out of memory: Killed process 9 (java) total-vm:2kB, anon-rss:2kB
(undated) Mon Nov 25 07:00:00 2026 Out of memory: Killed process 10 (java) total-vm:3kB, anon-rss:3kB
[ 99.2] Out of memory: Killed process 4242 (java) total-vm:9000000kB, anon-rss:3100000kB
over 10s: cpu user 50% system 0% iowait 0% steal 0% idle 50% | stalled on cpu 0s, io 0s, memory 0s | throttled n/a"

# Memory samples taken during the job: the report names the lowest and the last, with their top
# processes. A stand-in `ps` and meminfo make the sampled line reproducible.
printf 'MemTotal: 8388608 kB\nMemAvailable: 524288 kB\nShmem: 2097152 kB\n' > "$fixture/sampled-meminfo"
cat > "$fixture/ps" <<'STUB'
#!/usr/bin/env bash
echo '3145728 java'
echo '1048576 conformance'
STUB
chmod +x "$fixture/ps"
mkdir -p "$fixture/sampled/proc"
cp "$fixture/sampled-meminfo" "$fixture/sampled/proc/meminfo"
(
    root=$fixture/sampled
    RUNNER_PROFILE_ROOT=$root RUNNER_PROFILE_PS=$fixture/ps RUNNER_PROFILE_SAMPLE_SECONDS=0 \
        bash -c 'source <(sed -n "/^memory_sample()/,/^}/p" "$1"); root=$RUNNER_PROFILE_ROOT; memory_sample' _ \
        "$repo/scripts/runner-profile.sh"
) | sed 's/^\([0-9]*\)\t[0-9:]* /\1\t00:00:00 /' > "$fixture/sample.line"
printf '900\t10:00:00 available 900M, tmpfs 10M | java 1000M; \n512\t10:00:15 available 512M, tmpfs 2048M | java 3072M; \n700\t10:00:30 available 700M, tmpfs 10M | java 2000M; \n' \
    > "$fixture/memory.state.memory"
cp "$fixture/root.state" "$fixture/memory.state"
RUNNER_PROFILE_ROOT=$fixture/root-end "$repo/scripts/runner-profile.sh" report "$fixture/memory.state" > "$fixture/memory.out"
expect memory "machine: 2 cpus (Stand-in CPU @ 2.00GHz), cpu quota max, memory 8.0 GiB, swap 1.0 of 2.0 GiB used
lowest available memory: 10:00:15 available 512M, tmpfs 2048M | java 3072M; 
last memory sample: 10:00:30 available 700M, tmpfs 10M | java 2000M; 
over 10s: cpu user 50% system 0% iowait 0% steal 0% idle 50% | stalled on cpu 0s, io 0s, memory 0s | throttled n/a"
if [[ $(cat "$fixture/sample.line") != "512	00:00:00 available 512M, tmpfs 2048M | java 3072M; conformance 1024M; " ]]; then
    echo "sample line: got $(cat "$fixture/sample.line")" >&2
    failures=$((failures + 1))
fi

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
echo "runner profile: 6 cases passed"
