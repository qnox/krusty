#!/usr/bin/env python3
"""Runs one corpus build and records what it cost: wall time, CPU time, and the peak resident memory
of every process the build started, summed and by kind.

usage: measure.py <metrics.json> <command>...

The build's processes include the daemons it starts: Gradle's, the Kotlin compile daemon, and every
krusty process. A daemon outlives the build that started it, so this process becomes the child
subreaper of the build: an orphaned daemon stays a descendant, is sampled with the rest, and is
stopped when the command exits, so the next build starts and pays for its own. Memory is the sum of
the descendants' resident sets, sampled every 100 ms; CPU time is that of every descendant reaped.
Exits with the command's status.
"""
import ctypes
import json
import os
import resource
import signal
import subprocess
import sys
import threading
import time

PR_SET_CHILD_SUBREAPER = 36
KINDS = ('gradle', 'kotlin_daemon', 'krusty', 'other')


def children():
    """{pid: parent pid} of every live process."""
    found = {}
    for entry in os.listdir('/proc'):
        if entry.isdigit():
            try:
                with open(f'/proc/{entry}/stat') as f:
                    # The command name is parenthesized and may hold spaces; fields resume after it.
                    found[int(entry)] = int(f.read().rsplit(')', 1)[1].split()[1])
            except (OSError, IndexError, ValueError):
                pass
    return found


def descendants(root):
    parents = children()
    found, frontier = set(), {root}
    while frontier:
        frontier = {pid for pid, parent in parents.items() if parent in frontier and pid not in found}
        found |= frontier
    return found


def kind(pid):
    try:
        with open(f'/proc/{pid}/cmdline', 'rb') as f:
            argv = [a.decode(errors='replace') for a in f.read().split(b'\0') if a]
    except OSError:
        return None
    if not argv:
        return None
    line = ' '.join(argv)
    if 'KotlinCompileDaemon' in line:
        return 'kotlin_daemon'
    if os.path.basename(argv[0]) == 'krusty':
        return 'krusty'
    # The wrapper, the launcher, the daemon and its workers.
    if any(m in line for m in ('org.gradle.', 'gradle-wrapper', 'gradle-cli-main')):
        return 'gradle'
    return 'other'


def resident(pid):
    try:
        with open(f'/proc/{pid}/status') as f:
            for line in f:
                if line.startswith('VmRSS:'):
                    return int(line.split()[1]) * 1024
    except OSError:
        pass
    return 0


def sample(root, peak):
    total, by_kind = 0, dict.fromkeys(KINDS, 0)
    for pid in descendants(root):
        k = kind(pid)
        if k:
            size = resident(pid)
            total += size
            by_kind[k] += size
    peak['total'] = max(peak['total'], total)
    for k, size in by_kind.items():
        peak[k] = max(peak[k], size)


def stop_leftovers(root):
    """Stops the daemons the build left behind and reaps every descendant, so their CPU time counts."""
    for sig in (signal.SIGTERM, signal.SIGKILL):
        left = descendants(root)
        for pid in left:
            try:
                os.kill(pid, sig)
            except OSError:
                pass
        deadline = time.monotonic() + 10
        while descendants(root) and time.monotonic() < deadline:
            reap()
            time.sleep(0.1)
    reap()


def reap():
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return


def main():
    out, command = sys.argv[1], sys.argv[2:]
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), 'cannot become the child subreaper')
    root = os.getpid()
    peak = dict.fromkeys(('total',) + KINDS, 0)
    done = threading.Event()

    def sampler():
        while not done.is_set():
            sample(root, peak)
            done.wait(0.1)

    thread = threading.Thread(target=sampler, daemon=True)
    start = time.monotonic()
    process = subprocess.Popen(command)
    thread.start()
    code = process.wait()
    wall = time.monotonic() - start
    done.set()
    thread.join()
    stop_leftovers(root)
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    metrics = dict(exit=code, wall_seconds=round(wall, 1), cpu_seconds=round(usage.ru_utime + usage.ru_stime, 1),
                   peak_rss_mb={k: round(v / 2**20) for k, v in peak.items()})
    with open(out, 'w') as f:
        json.dump(metrics, f, indent=1)
    # A command killed by a signal exits the way a shell reports it.
    sys.exit(code if code >= 0 else 128 - code)


if __name__ == '__main__':
    main()
