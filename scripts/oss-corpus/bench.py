#!/usr/bin/env python3
"""The warm benchmark: krusty against kotlinc with a warm Gradle daemon, on the compilations of one
corpus project that krusty builds.

usage: bench.py plan <logs dir> <name> <Gradle root> <plan.json>
       bench.py session <plan.json> <kotlinc|krusty> <result.json> <Gradle argument>...
       bench.py report <bench dir> <name>

`plan` reads the krusty probe build `bench.sh` runs first: its compile-task inventory
(`<name>.probe.compilations.tsv`), task times (`<name>.probe.tasks.tsv`), source inventory
(`<name>.probe.sources.tsv`) and log (`<name>.probe.log`). The benchmarked compilations are those
krusty compiled: their `WithKrusty` replacement ran and succeeded. A compilation krusty fails, one
the plugin leaves to kotlinc, and one that did not run are left out, so both compilers build the
same task set and both builds succeed. The edited file is the median-sized Kotlin source of the
benchmarked main (non-test) compilation with the most sources; `$BENCH_EDIT` (a path relative to
the Gradle root) overrides it.

`session` runs every build of one compiler against one Gradle daemon, which it starts and stops:
`$BENCH_WARMUPS` unrecorded clean builds (default 2; they may download), `$BENCH_CLEAN_RUNS`
recorded clean builds (default 3, `--rerun-tasks`), then two edit loops of `$BENCH_EDIT_RUNS`
recorded builds each (default 5), after one unrecorded edit. A `body` edit changes the result of a
private top-level function appended to the edited file, which leaves its ABI alone; an `abi` edit
replaces a public top-level function with one of a new name each time. Recorded builds run
offline and without the build cache. The original file is restored afterwards.

Like `measure.py`, the session is the child subreaper of everything it starts, so the Gradle
daemon, the Kotlin daemon and every krusty process stay its descendants. Each build records its wall
time, the summed time of the benchmarked compilations' tasks, the peak of the descendants' summed
resident memory during the build (sampled every 100 ms, split by kind; the warm daemons count), and
what stays resident once the build is done.

`report` combines `<bench dir>/<name>/{plan,kotlinc,krusty}.json` into `<bench dir>/<name>.json` and
prints a Markdown table.
"""
import ctypes
import json
import os
import re
import statistics
import subprocess
import sys
import threading
import time

import compare
import measure

SCENARIOS = ('clean', 'body', 'abi')


def env_int(name, default):
    return int(os.environ.get(name, default))


# --- plan -----------------------------------------------------------------------------------------

def read_rows(path):
    try:
        with open(path) as f:
            return [line.rstrip('\n').split('\t') for line in f if line.strip()]
    except OSError:
        return []


def krusty_compiled(tasks_tsv, compilations, log):
    """The compile tasks whose krusty replacement ran and succeeded, and the plugin did not leave
    to kotlinc."""
    outcome = {row[0]: row[3] for row in read_rows(tasks_tsv) if len(row) == 4}
    left = compare.fallbacks(log)
    return sorted(t for t, _ in compilations
                  if outcome.get(t + 'WithKrusty') == 'executed' and t not in left)


def is_test(task):
    return re.search(r'Test', task.rsplit(':', 1)[-1]) is not None


def pick_edit(tasks, sources_tsv, root):
    """(task, file relative to the Gradle root): the median-sized hand-written Kotlin source of the
    main compilation with the most of them, or of any compilation when every one is a test."""
    root = os.path.realpath(root)
    by_task = {}
    for row in read_rows(sources_tsv):
        if len(row) != 2 or row[0] not in tasks:
            continue
        path = os.path.realpath(row[1])
        rel = os.path.relpath(path, root)
        if rel.startswith('..') or '/build/' in '/' + rel or not os.path.isfile(path):
            continue
        by_task.setdefault(row[0], []).append(rel)
    candidates = {t: files for t, files in by_task.items() if not is_test(t)} or by_task
    if not candidates:
        return None, None
    task = max(sorted(candidates), key=lambda t: len(candidates[t]))
    files = sorted(candidates[task], key=lambda rel: (os.path.getsize(os.path.join(root, rel)), rel))
    return task, files[len(files) // 2]


def plan(logs, name, root, out):
    prefix = os.path.join(logs, f'{name}.probe')
    tasks = krusty_compiled(prefix + '.tasks.tsv', compare.compilations(prefix + '.compilations.tsv'),
                            prefix + '.log')
    override = os.environ.get('BENCH_EDIT')
    if override:
        edit_task, edit = None, override
    else:
        edit_task, edit = pick_edit(tasks, prefix + '.sources.tsv', root)
    result = dict(project=name, tasks=tasks, edit=edit, edit_task=edit_task,
                  compilations=len(compare.compilations(prefix + '.compilations.tsv')))
    with open(out, 'w') as f:
        json.dump(result, f, indent=1)
    print(f'{name}: {len(tasks)} of {result["compilations"]} compilations built by krusty; edit {edit}')


# --- session --------------------------------------------------------------------------------------

def edited(original, kind, n):
    """The edited file's text for the `n`th edit of a kind."""
    text = original if original.endswith('\n') else original + '\n'
    if kind == 'body':
        return text + f'\nprivate fun corpusBenchBody(): Int = {n}\n'
    return text + f'\npublic fun corpusBenchAbi{n}(): Int = {n}\n'


class Sampler:
    """Samples the summed resident memory of this process's descendants until stopped."""

    def __init__(self, root):
        self.root = root
        self.peak = dict.fromkeys(('total',) + measure.KINDS, 0)
        self.done = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)

    def run(self):
        while not self.done.is_set():
            measure.sample(self.root, self.peak)
            self.done.wait(0.1)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.done.set()
        self.thread.join()
        measure.sample(self.root, self.peak)


def resident(root):
    now = dict.fromkeys(('total',) + measure.KINDS, 0)
    measure.sample(root, now)
    return now


def mb(sizes):
    return {k: round(v / 2**20) for k, v in sizes.items()}


class Session:
    def __init__(self, plan_doc, mode, gradle_args, log, times):
        self.plan = plan_doc
        self.mode = mode
        self.gradle_args = gradle_args
        self.log = log
        self.times = times
        self.root = os.getpid()
        self.runs = {s: [] for s in SCENARIOS}

    def build(self, *extra):
        command = ['./gradlew', '--daemon', *self.gradle_args, *extra, *self.plan['tasks']]
        env = dict(os.environ, CORPUS_TASK_TIMES=self.times)
        with open(self.log, 'a') as log:
            log.write(f'\n$ {" ".join(command)}\n')
            log.flush()
            start = time.monotonic()
            with Sampler(self.root) as sampler:
                code = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, env=env).returncode
            wall = time.monotonic() - start
        # Let the daemons settle: what they keep once the build is over is the price of keeping them warm.
        time.sleep(1)
        times = compare.task_seconds(self.times)
        compiled = [compare.compile_seconds(times, t, self.mode) for t in self.plan['tasks']]
        return dict(exit=code, wall_seconds=round(wall, 2),
                    compile_seconds=round(sum(s for s in compiled if s is not None), 2),
                    peak_rss_mb=mb(sampler.peak), resident_after_mb=mb(resident(self.root)))

    def run(self):
        runs = self.runs
        recorded = ('--offline', '--no-build-cache')

        def record(scenario, *extra):
            r = self.build(*recorded, *extra)
            runs[scenario].append(r)
            if r['exit'] != 0:
                raise RuntimeError(f'{self.mode} {scenario} build {len(runs[scenario])} exited {r["exit"]}')

        for i in range(env_int('BENCH_WARMUPS', 2)):
            # The first warm-up may download what the build needs; the rest warm the daemons.
            r = self.build('--no-build-cache', '--rerun-tasks')
            if r['exit'] != 0:
                raise RuntimeError(f'{self.mode} warm-up build {i + 1} exited {r["exit"]}')
        for _ in range(env_int('BENCH_CLEAN_RUNS', 3)):
            record('clean', '--rerun-tasks')
        path = os.path.join(os.getcwd(), self.plan['edit'])
        with open(path) as f:
            original = f.read()
        n = 0
        try:
            for kind in ('body', 'abi'):
                for i in range(1 + env_int('BENCH_EDIT_RUNS', 5)):
                    n += 1
                    with open(path, 'w') as f:
                        f.write(edited(original, kind, n))
                    if i == 0:
                        # The first edit of a kind adds or replaces the function and is not recorded.
                        r = self.build(*recorded)
                        if r['exit'] != 0:
                            raise RuntimeError(f'{self.mode} first {kind} edit exited {r["exit"]}')
                    else:
                        record(kind)
        finally:
            with open(path, 'w') as f:
                f.write(original)


def session(plan_path, mode, out, gradle_args):
    with open(plan_path) as f:
        plan_doc = json.load(f)
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(measure.PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), 'cannot become the child subreaper')
    stem = os.path.splitext(out)[0]
    for suffix in ('.log', '.tasks.tsv'):
        if os.path.exists(stem + suffix):
            os.remove(stem + suffix)
    s = Session(plan_doc, mode, gradle_args, stem + '.log', stem + '.tasks.tsv')
    error = None
    try:
        if not plan_doc['tasks'] or not plan_doc['edit']:
            raise RuntimeError('krusty built no compilation to benchmark')
        s.run()
    except RuntimeError as e:
        error = str(e)
    finally:
        subprocess.run(['./gradlew', '--stop'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        measure.stop_leftovers(os.getpid())
    result = dict(mode=mode, runs=s.runs, error=error)
    with open(out, 'w') as f:
        json.dump(result, f, indent=1)
    if result['error']:
        print(f'{mode}: {result["error"]}', file=sys.stderr)
        sys.exit(1)


# --- report ---------------------------------------------------------------------------------------

def stats(runs):
    """The median wall and compile time and the highest peak memory of a scenario's runs."""
    if not runs:
        return None
    return dict(runs=len(runs),
                wall_seconds=round(statistics.median(r['wall_seconds'] for r in runs), 2),
                wall_seconds_range=[min(r['wall_seconds'] for r in runs), max(r['wall_seconds'] for r in runs)],
                compile_seconds=round(statistics.median(r['compile_seconds'] for r in runs), 2),
                peak_rss_mb=max(r['peak_rss_mb']['total'] for r in runs),
                peak_rss_mb_by_kind={k: max(r['peak_rss_mb'][k] for r in runs) for k in measure.KINDS},
                resident_after_mb=max(r['resident_after_mb']['total'] for r in runs))


def report(bench, name):
    def load(part):
        try:
            with open(os.path.join(bench, name, part + '.json')) as f:
                return json.load(f)
        except OSError:
            return None

    plan_doc = load('plan') or {}
    modes = {}
    for mode in ('kotlinc', 'krusty'):
        doc = load(mode)
        if doc is None:
            modes[mode] = dict(error='not run')
            continue
        modes[mode] = dict(error=doc['error'], **{s: stats(doc['runs'][s]) for s in SCENARIOS})
    result = dict(project=name, tasks=plan_doc.get('tasks', []), compilations=plan_doc.get('compilations'),
                  edit=plan_doc.get('edit'), modes=modes)
    with open(os.path.join(bench, name + '.json'), 'w') as f:
        json.dump(result, f, indent=1)

    print(f'## {name}\n')
    print(f'{len(result["tasks"])} of {result["compilations"]} compilations (those krusty builds); '
          f'edits in `{result["edit"]}`.\n')
    print('| Scenario | Compiler | Wall (median) | Compile tasks | Peak RSS | Resident after |')
    print('| --- | --- | ---: | ---: | ---: | ---: |')
    for scenario in SCENARIOS:
        for mode in ('kotlinc', 'krusty'):
            s = modes[mode].get(scenario)
            if s:
                print(f'| {scenario} | {mode} | {s["wall_seconds"]} s | {s["compile_seconds"]} s '
                      f'| {s["peak_rss_mb"]} MB | {s["resident_after_mb"]} MB |')
            else:
                print(f'| {scenario} | {mode} | {modes[mode]["error"] or "-"} | | | |')


def main():
    command, args = sys.argv[1], sys.argv[2:]
    if command == 'plan':
        plan(*args)
    elif command == 'session':
        session(args[0], args[1], args[2], args[3:])
    elif command == 'report':
        report(*args)
    else:
        sys.exit(__doc__)


if __name__ == '__main__':
    main()
