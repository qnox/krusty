#!/usr/bin/env python3
"""Compares one corpus project's class outputs: kotlinc (its own Gradle build) vs krusty (the same
build with the krusty Gradle plugin).

usage: compare.py <work dir> <name> <reference build exit code> [<kotlinc test build exit code>
                  <krusty test build exit code>]

Reads <work dir>/out/<name>/{kotlinc,krusty}, <work dir>/logs/<name>.kotlinc.compilations.tsv (the
reference build's compile tasks and their output directories), <work dir>/logs/<name>.krusty.log
(fallback reasons and the first error of each failed compile), and each build's
<work dir>/logs/<name>.<mode>.metrics.json and <name>.<mode>.tasks.tsv (what the build cost, and when
each task ran), and <work dir>/logs/<name>.krusty.folded when the krusty build was sampled (its
call stacks, ranked into the functions krusty spends the most time in). With the test builds' exit
codes, it also compares the project's tests run on each build's classes (`testruns.py`). Writes
<work dir>/out/<name>/compare.json and prints one line per compilation.
"""
import glob
import json
import os
import re
import sys

import hotspots
import summarize
import testruns


def fallbacks(log_path):
    """Compile tasks the krusty plugin left to kotlinc, with the reason it logged: {task path: reason}."""
    found = {}
    try:
        text = open(log_path, errors='replace').read()
    except OSError:
        return found
    lines = text.splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"krusty: task '([^']*:compile\w*Kotlin\w*)' is left to kotlinc: (.*)", line)
        if not m:
            continue
        task, reason = m.groups()
        detail = [l.strip() for l in lines[i + 1:i + 6] if l.startswith('  ')]
        if detail:
            reason += ' (' + '; '.join(detail) + ')'
        found[task] = reason
    return found


ERROR_LINE = re.compile(r'^(e: |error: )|: error: |panicked at ')
TASK_HEADER = re.compile(r'^> Task (\S+)')


def failures(log_path, checkout):
    """The compile tasks the krusty build ran, and the first error each logged: ({task path},
    {task path: message}). Gradle heads every task it runs, and its output, with `> Task <path>`;
    the replacement task is the compile task's path with `WithKrusty` appended. Paths inside the
    checkout are shown relative to it."""
    ran, found = set(), {}
    try:
        lines = open(log_path, errors='replace').read().splitlines()
    except OSError:
        return ran, found
    task = None
    prefixes = ('file://' + checkout + '/', checkout + '/')
    for line in lines:
        header = TASK_HEADER.match(line)
        if header:
            task = header.group(1).split(' ')[0]
            if task.endswith('WithKrusty'):
                task = task[:-len('WithKrusty')]
            ran.add(task)
            continue
        if task and task not in found and ERROR_LINE.search(line):
            message = line.strip()
            for prefix in prefixes:
                message = message.replace(prefix, '')
            found[task] = message[:300]
    return ran, found


def build_failure(log_path):
    """The innermost message of the first `* What went wrong:` block of a Gradle log, or None."""
    try:
        lines = open(log_path, errors='replace').read().splitlines()
    except OSError:
        return None
    for i, line in enumerate(lines):
        if line.startswith('* What went wrong:'):
            # Each cause is a `>` line indented under the one it explains; other lines continue
            # the message above them, such as a link to the Gradle documentation.
            innermost, depth = None, -1
            for detail in lines[i + 1:]:
                if not detail.strip() or detail.startswith('* '):
                    break
                indent = len(detail) - len(detail.lstrip())
                if innermost is None:
                    innermost = detail.strip()
                elif detail.lstrip().startswith('>') and indent > depth:
                    innermost, depth = detail.strip().lstrip('> ').strip(), indent
            return innermost[:300] if innermost else None
    return None


def compilations(manifest):
    """The compile tasks the reference build ran, as (task path, output directory relative to the
    Gradle root), from the manifest `corpus.init.gradle` writes. Each task is one compilation: a
    Kotlin Multiplatform JVM target's main and test compilations write `<target>/main` and
    `<target>/test` and stay separate."""
    try:
        with open(manifest) as f:
            return sorted(tuple(line.rstrip('\n').split('\t')) for line in f if line.strip())
    except OSError:
        return []


def task_seconds(path):
    """{task path: seconds it ran} from the task times `corpus.init.gradle` records."""
    seconds = {}
    try:
        with open(path) as f:
            for line in f:
                fields = line.rstrip('\n').split('\t')
                if len(fields) == 4:
                    seconds[fields[0]] = (int(fields[2]) - int(fields[1])) / 1000
    except OSError:
        pass
    return seconds


def compile_seconds(times, task, mode):
    """A compilation's compile time in one build, or None when none of its tasks ran. kotlinc's is
    the Kotlin task; krusty's adds the replacement and its support check, and the Kotlin task, which
    is kotlinc's compile when the plugin leaves the compilation to it."""
    tasks = [task] if mode == 'kotlinc' else [task, task + 'KrustyCheck', task + 'WithKrusty']
    ran = [times[t] for t in tasks if t in times]
    return round(sum(ran), 2) if ran else None


def build_cost(work, name, mode, rows):
    """What one build cost: `measure.py`'s record of the whole build, and the compile time of every
    compared compilation and of those krusty built, or None when the build was not measured."""
    try:
        with open(os.path.join(work, 'logs', f'{name}.{mode}.metrics.json')) as f:
            cost = json.load(f)
    except OSError:
        return None
    cost.pop('exit', None)
    cost['compile_seconds'] = round(sum(r['seconds'][mode] or 0 for r in rows), 1)
    cost['built_compile_seconds'] = round(sum(r['seconds'][mode] or 0 for r in rows if r['built']), 1)
    return cost


def sampled_hotspots(folded):
    """The functions the sampled krusty build spent the most time in, or None when it was not
    sampled."""
    if not os.path.exists(folded):
        return None
    return hotspots.rank(hotspots.read_folded(folded))


def files(d):
    return {os.path.relpath(p, d): p for p in glob.glob(d + '/**/*', recursive=True) if os.path.isfile(p)}


def same_bytes(left, right):
    with open(left, 'rb') as a, open(right, 'rb') as b:
        return a.read() == b.read()


def lines_of_code(checkout):
    """Non-blank lines of .kt sources (build scripts excluded) and of .java sources."""
    counts = {'kotlin': 0, 'java': 0}
    for root, dirs, names in os.walk(checkout):
        dirs[:] = [d for d in dirs if d not in ('.git', 'build', '.gradle')]
        for name in names:
            kind = 'kotlin' if name.endswith('.kt') else 'java' if name.endswith('.java') else None
            if kind:
                with open(os.path.join(root, name), errors='replace') as f:
                    counts[kind] += sum(1 for line in f if line.strip())
    return counts


def compare(work, name):
    """The compared compilations, and the listed compile tasks for which kotlinc wrote no class
    (a source set without sources): those are reported apart, with any classes krusty wrote."""
    kotlinc, krusty = os.path.join(work, 'out', name, 'kotlinc'), os.path.join(work, 'out', name, 'krusty')
    log = os.path.join(work, 'logs', name + '.krusty.log')
    found = fallbacks(log)
    ran, errors = failures(log, os.path.join(work, 'src', name))
    # A krusty build that fails before running any compile (configuration, plugin resolution)
    # says why once, for every compile it never reached.
    not_run = 'not run: a task it depends on failed'
    if not any(task in ran for task, _ in compilations(os.path.join(work, 'logs', name + '.kotlinc.compilations.tsv'))):
        failure = build_failure(log)
        if failure:
            not_run = 'not run: the krusty build failed: ' + failure
    times = {mode: task_seconds(os.path.join(work, 'logs', f'{name}.{mode}.tasks.tsv'))
             for mode in ('kotlinc', 'krusty')}
    rows, empty = [], []
    for task, rel in compilations(os.path.join(work, 'logs', name + '.kotlinc.compilations.tsv')):
        reference = files(os.path.join(kotlinc, rel))
        candidate = files(os.path.join(krusty, rel))
        if not any(k.endswith('.class') for k in reference):
            empty.append(dict(task=task, dir=rel,
                              krusty_classes=sum(1 for k in candidate if k.endswith('.class'))))
            continue
        fallback = found.get(task)
        # A compile left to kotlinc wrote kotlinc's classes: it is a miss, not a match.
        built = fallback is None and any(k.endswith('.class') for k in candidate)
        classes = sum(1 for k in reference if k.endswith('.class'))
        identical = sum(1 for k, p in reference.items()
                        if k.endswith('.class') and k in candidate and same_bytes(p, candidate[k]))
        error = None
        if not built and fallback is None:
            # With --continue, Gradle skips a compile whose dependency failed.
            error = errors.get(task, 'krusty wrote no class' if task in ran else not_run)
        rows.append(dict(task=task, dir=rel, built=built, fallback=fallback, error=error, classes=classes,
                         identical=identical if built else 0,
                         only_kotlinc=len(set(reference) - set(candidate)) if built else None,
                         only_krusty=len(set(candidate) - set(reference)) if built else None,
                         seconds={mode: compile_seconds(times[mode], task, mode) for mode in times}))
    return rows, empty


def main():
    work, name, reference_exit = sys.argv[1], sys.argv[2], int(sys.argv[3])
    test_exits = dict(zip(('kotlinc', 'krusty'), map(int, sys.argv[4:6])))
    rows, empty = compare(work, name)
    summary = dict(
        name=name,
        reference_exit=reference_exit,
        loc=lines_of_code(os.path.join(work, 'src', name)),
        compilations=len(rows),
        built=sum(r['built'] for r in rows),
        classes=sum(r['classes'] for r in rows),
        identical=sum(r['identical'] for r in rows),
        cost={mode: build_cost(work, name, mode, rows) for mode in ('kotlinc', 'krusty')},
        hotspots=sampled_hotspots(os.path.join(work, 'logs', f'{name}.krusty.folded')),
        tests=testruns.compare(work, name, rows, test_exits) if test_exits else None,
        rows=rows,
        no_reference_classes=empty,
    )
    with open(os.path.join(work, 'out', name, 'compare.json'), 'w') as f:
        json.dump(summary, f, indent=1)
    print(f"{name}: reference exit {reference_exit}; compilations {summary['built']}/{summary['compilations']} "
          f"built by krusty; classes identical {summary['identical']}/{summary['classes']}")
    for mode, cost in summary['cost'].items():
        if cost:
            print(f"  {mode} build: {cost['wall_seconds']} s wall, {cost['cpu_seconds']} s CPU, "
                  f"peak {cost['peak_rss_mb']['total']} MB; compile {cost['compile_seconds']} s, "
                  f"{cost['built_compile_seconds']} s in compilations krusty built")
    if summary['tests'] and summary['tests']['unavailable']:
        for mode, entry in summary['tests']['unavailable'].items():
            for why in summarize.unavailable_reasons(entry):
                print(f"  tests unavailable: the {mode} test build {why}"
                      + (f": {', '.join(entry['examples'])}" if entry.get('examples') else ''))
    elif summary['tests']:
        for scope, label in (('total', 'all test tasks'), ('krusty_classes', 'tasks on krusty classes only')):
            t = summary['tests'][scope]
            print(f"  tests, {label} ({t['tasks']}): {t['passed']}/{t['passed_kotlinc']} passing on kotlinc's "
                  f"classes pass on krusty's; {t['failed']} fail, {t['skipped']} skipped, {t['not_run']} not run")
    for r in rows:
        state = 'ok ' if r['built'] else ('KTC' if r['fallback'] else 'ERR')
        print(f"  {state} {r['task']} ({r['dir']}): {r['identical']}/{r['classes']}"
              + (f" left to kotlinc: {r['fallback']}" if r['fallback'] else '')
              + (f" error: {r['error']}" if r['error'] else '')
              + (f" only_kotlinc={r['only_kotlinc']} only_krusty={r['only_krusty']}" if r['built'] else ''))
    for r in empty:
        print(f"  --- {r['task']} ({r['dir']}): kotlinc wrote no class; krusty wrote {r['krusty_classes']}")


if __name__ == '__main__':
    main()
