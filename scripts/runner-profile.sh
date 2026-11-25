#!/usr/bin/env bash
# Describe the machine a job ran on and how contended its CPUs, disk and memory were.
#
#   runner-profile.sh start <state file>    record the CPU and pressure counters
#   runner-profile.sh report <state file>   print the machine and the counters' change since start
#
# One label can name machines of different speeds. The report says which machine a job had, how
# much of its CPU time the hypervisor gave to other guests (steal), how long tasks waited for CPU,
# disk and memory (pressure stall), and how long the job's cgroup was throttled, so a slow job can
# be attributed to its machine rather than to the code it ran. RUNNER_PROFILE_ROOT prefixes /proc
# and /sys for the script's own tests.
set -euo pipefail

root=${RUNNER_PROFILE_ROOT:-}

# `cpu` line of /proc/stat: user nice system idle iowait irq softirq steal, in clock ticks.
cpu_ticks() {
    awk '$1 == "cpu" { print $2, $3, $4, $5, $6, $7, $8, $9; exit }' "$root/proc/stat"
}

# Microseconds some task stalled on one resource since boot, or `-` where the kernel has no PSI.
pressure_usec() {
    local file=$root/proc/pressure/$1
    if [[ -r $file ]]; then
        awk '$1 == "some" { for (i = 2; i <= NF; i++) if ($i ~ /^total=/) { sub(/^total=/, "", $i); total = $i } }
             END { print (total == "") ? "-" : total }' "$file"
    else
        echo -
    fi
}

# Microseconds this job's cgroup was throttled by its CPU quota, or `-` where its cpu.stat has no
# throttling counter, as without a cgroup v2 cpu controller or in the root cgroup.
throttled_usec() {
    local file=$root/sys/fs/cgroup/cpu.stat
    if [[ -r $file ]]; then
        awk '$1 == "throttled_usec" { usec = $2 } END { print (usec == "") ? "-" : usec }' "$file"
    else
        echo -
    fi
}

counters() {
    echo "$(awk '{ printf "%d", $1 }' "$root/proc/uptime") $(cpu_ticks) $(pressure_usec cpu) $(pressure_usec io) $(pressure_usec memory) $(throttled_usec)"
}

machine() {
    local cpus model quota mem_kb swap_total swap_free
    cpus=$(grep -c '^processor' "$root/proc/cpuinfo")
    model=$(awk -F': *' '$1 ~ /^model name/ { print $2; exit }' "$root/proc/cpuinfo")
    quota=max
    if [[ -r $root/sys/fs/cgroup/cpu.max ]]; then
        quota=$(awk '{ print ($1 == "max") ? "max" : sprintf("%.2f cpus", $1 / $2) }' "$root/sys/fs/cgroup/cpu.max")
    fi
    mem_kb=$(awk '$1 == "MemTotal:" { print $2 }' "$root/proc/meminfo")
    swap_total=$(awk '$1 == "SwapTotal:" { print $2 }' "$root/proc/meminfo")
    swap_free=$(awk '$1 == "SwapFree:" { print $2 }' "$root/proc/meminfo")
    awk -v cpus="$cpus" -v model="$model" -v quota="$quota" -v mem="$mem_kb" \
        -v swap_total="$swap_total" -v swap_free="$swap_free" 'BEGIN {
        printf "machine: %d cpus (%s), cpu quota %s, memory %.1f GiB, swap %.1f of %.1f GiB used\n",
            cpus, model, quota, mem / 1048576, (swap_total - swap_free) / 1048576, swap_total / 1048576
    }'
}

# What the kernel killed for memory since `since` (seconds of uptime when this job started), newest
# last, so an exit-137 job names its culprit and the size it had. A long-lived runner's kernel log
# also holds the kills of earlier jobs, which the timestamp in front of each line drops. A line with
# no uptime timestamp cannot be placed, so it is kept and marked undated.
# RUNNER_PROFILE_DMESG replaces the `dmesg` command for the script's own tests.
memory_kills() {
    local since=$1 dmesg=${RUNNER_PROFILE_DMESG:-dmesg}
    { "$dmesg" 2>/dev/null || sudo -n "$dmesg" 2>/dev/null || true; } \
        | grep -iE 'out of memory|oom-kill|killed process' \
        | awk -v since="$since" '
            match($0, /^\[ *[0-9]+/) { if (substr($0, 2, RLENGTH - 1) + 0 >= since) print; next }
            { print "(undated) " $0 }' \
        | tail -n 10 || true
}

# One line of what memory looked like: the memory still available, what tmpfs holds, and the five
# processes with the most resident memory. RUNNER_PROFILE_PS replaces `ps` for the script's tests.
memory_sample() {
    local ps=${RUNNER_PROFILE_PS:-ps}
    awk -v now="$(date +%T)" -v tops="$("$ps" -eo rss=,comm= --sort=-rss | head -n 5 \
        | awk '{ printf "%s %dM; ", $2, $1 / 1024 }')" '
        $1 == "MemAvailable:" { available = int($2 / 1024) }
        $1 == "Shmem:" { shmem = int($2 / 1024) }
        END { printf "%d\t%s available %dM, tmpfs %dM | %s\n", available, now, available, shmem, tops }' \
        "$root/proc/meminfo"
}

# Samples memory every RUNNER_PROFILE_SAMPLE_SECONDS (15) until the job ends, so a job the kernel
# killed still says what held its memory just before.
sample_memory() {
    while true; do
        memory_sample >> "$state.memory" || true
        sleep "${RUNNER_PROFILE_SAMPLE_SECONDS:-15}"
    done
}

usage() {
    echo "usage: $0 start|report|sample <state file>" >&2
    exit 2
}

[[ $# -eq 2 ]] || usage
state=$2
case $1 in
    start)
        counters > "$state"
        : > "$state.memory"
        # A real job samples in the background; the script's own tests read stand-in trees.
        if [[ -z ${RUNNER_PROFILE_ROOT:-} ]]; then
            setsid nohup "$0" sample "$state" > /dev/null 2>&1 &
            echo $! > "$state.sampler"
        fi
        ;;
    sample)
        sample_memory
        ;;
    report)
        [[ -r $state ]] || { echo "no runner profile was started at $state" >&2; exit 1; }
        machine
        [[ ! -r $state.sampler ]] || kill "$(cat "$state.sampler")" 2> /dev/null || true
        if [[ -s $state.memory ]]; then
            printf 'lowest available memory: %s\nlast memory sample: %s\n' \
                "$(sort -n "$state.memory" | head -n 1 | cut -f2-)" "$(tail -n 1 "$state.memory" | cut -f2-)"
        fi
        kills=$(memory_kills "$(cut -d' ' -f1 "$state")")
        [[ -z $kills ]] || printf 'memory kills recorded by the kernel:\n%s\n' "$kills"
        echo "$(cat "$state") $(counters)" | awk '
            function seconds(before, after) {
                return (before == "-" || after == "-") ? "n/a" : sprintf("%.0fs", (after - before) / 1e6)
            }
            {
                total = 0
                for (i = 2; i <= 9; i++) { delta[i] = $(i + 13) - $i; total += delta[i] }
                if (total <= 0) total = 1
                printf "over %ds: cpu user %.0f%% system %.0f%% iowait %.0f%% steal %.0f%% idle %.0f%%",
                    $14 - $1,
                    100 * (delta[2] + delta[3]) / total, 100 * (delta[4] + delta[7] + delta[8]) / total,
                    100 * delta[6] / total, 100 * delta[9] / total, 100 * delta[5] / total
                printf " | stalled on cpu %s, io %s, memory %s | throttled %s\n",
                    seconds($10, $23), seconds($11, $24), seconds($12, $25), seconds($13, $26)
            }'
        ;;
    *)
        usage
        ;;
esac
