#!/usr/bin/env python3
"""Compares one corpus project's class outputs: kotlinc (its own Gradle build) vs krusty (the same
build with the krusty Gradle plugin).

usage: compare.py <work dir> <name> <reference build exit code>

Reads <work dir>/out/<name>/{kotlinc,krusty}, <work dir>/logs/<name>.kotlinc.compilations.tsv (the
reference build's compile tasks and their output directories) and <work dir>/logs/<name>.krusty.log
(fallback reasons and the first error of each failed compile). Writes
<work dir>/out/<name>/compare.json and prints one line per compilation.
"""
import glob
import json
import os
import re
import sys


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
            error = errors.get(task, 'krusty wrote no class' if task in ran
                               else 'not run: a task it depends on failed')
        rows.append(dict(task=task, dir=rel, built=built, fallback=fallback, error=error, classes=classes,
                         identical=identical if built else 0,
                         only_kotlinc=len(set(reference) - set(candidate)) if built else None,
                         only_krusty=len(set(candidate) - set(reference)) if built else None))
    return rows, empty


def main():
    work, name, reference_exit = sys.argv[1], sys.argv[2], int(sys.argv[3])
    rows, empty = compare(work, name)
    summary = dict(
        name=name,
        reference_exit=reference_exit,
        loc=lines_of_code(os.path.join(work, 'src', name)),
        compilations=len(rows),
        built=sum(r['built'] for r in rows),
        classes=sum(r['classes'] for r in rows),
        identical=sum(r['identical'] for r in rows),
        rows=rows,
        no_reference_classes=empty,
    )
    with open(os.path.join(work, 'out', name, 'compare.json'), 'w') as f:
        json.dump(summary, f, indent=1)
    print(f"{name}: reference exit {reference_exit}; compilations {summary['built']}/{summary['compilations']} "
          f"built by krusty; classes identical {summary['identical']}/{summary['classes']}")
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
