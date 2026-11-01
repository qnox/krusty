#!/usr/bin/env python3
"""Compares one corpus project's own test suite run on kotlinc's classes and on krusty's.

After each measured build, `run.sh` runs the project's JVM test tasks (`corpusTest`) in an
unmeasured test build of the same mode, before the compile outputs are moved, so the tests run on
exactly the classes that build compiled. `corpus.init.gradle` lists each test task in
`<work dir>/logs/<name>.<mode>.tests.tsv`: its path, its JUnit XML report directory, and the
compile tasks whose classes it runs. Its reports are moved to `<work dir>/out/<name>/<mode>-tests`.

A test is compared only when it passed on kotlinc's classes: a test the reference build fails or
skips says nothing about krusty. Each such test then passed, failed or was skipped on krusty's
classes, or did not run there at all (its task did not run, or ran without reporting it). A test
task runs on krusty's classes only when krusty built every compilation it runs; the totals for
those tasks are reported apart, since the others also run classes kotlinc compiled.
"""
import collections
import glob
import os
import xml.etree.ElementTree as ElementTree

# Names of the tests that passed on kotlinc's classes and not on krusty's, kept per test task.
EXAMPLES = 10


def inventory(manifest):
    """[(test task, report directory, [compile tasks])] from a test manifest, or None when the
    build wrote none."""
    try:
        with open(manifest) as f:
            lines = [line.rstrip('\n') for line in f if line.strip()]
    except OSError:
        return None
    rows = []
    for line in lines:
        task, reports, compiles = (line.split('\t') + [''])[:3]
        rows.append((task, reports, [c for c in compiles.split(',') if c]))
    return rows


def outcomes(directory):
    """{(class, test): 'passed' | 'failed' | 'skipped'} from the JUnit XML reports in a directory. A
    test reported more than once (a repeated or parameterized name) failed if any run failed, and
    was skipped only if every run was."""
    runs = collections.defaultdict(set)
    for path in sorted(glob.glob(os.path.join(directory, '*.xml'))):
        try:
            suite = ElementTree.parse(path).getroot()
        except ElementTree.ParseError:
            continue
        for case in suite.iter('testcase'):
            if case.find('failure') is not None or case.find('error') is not None:
                outcome = 'failed'
            elif case.find('skipped') is not None:
                outcome = 'skipped'
            else:
                outcome = 'passed'
            runs[case.get('classname', ''), case.get('name', '')].add(outcome)
    return {key: 'failed' if 'failed' in seen else 'passed' if 'passed' in seen else 'skipped'
            for key, seen in runs.items()}


def counts(results):
    tally = collections.Counter(results.values())
    return dict(passed=tally['passed'], failed=tally['failed'], skipped=tally['skipped'])


def totals(tasks):
    keys = ('passed_kotlinc', 'passed', 'failed', 'skipped', 'not_run')
    return dict(tasks=len(tasks), **{k: sum(t['krusty'][k] for t in tasks) for k in keys})


def compare(work, name, rows, exits):
    """The test comparison of one project, or None when the reference test build listed no test
    task. `rows` are compare.py's compilations, `exits` each test build's exit status."""
    logs = os.path.join(work, 'logs')
    reference = inventory(os.path.join(logs, f'{name}.kotlinc.tests.tsv'))
    if not reference:
        return None
    candidate = {task: reports for task, reports, _ in
                 inventory(os.path.join(logs, f'{name}.krusty.tests.tsv')) or []}
    built = {r['task']: r['built'] for r in rows}
    out = os.path.join(work, 'out', name)
    tasks = []
    for task, reports, compiles in reference:
        kotlinc = outcomes(os.path.join(out, 'kotlinc-tests', reports))
        krusty = outcomes(os.path.join(out, 'krusty-tests', candidate[task])) if task in candidate else {}
        passing = [key for key, outcome in sorted(kotlinc.items()) if outcome == 'passed']
        compared = collections.Counter(krusty.get(key, 'not_run') for key in passing)
        lost = [f'{cls}.{test}' for cls, test in passing if krusty.get((cls, test)) != 'passed']
        tasks.append(dict(
            task=task,
            compilations=compiles,
            # Every compilation it runs was built by krusty: the task ran no class kotlinc compiled.
            krusty_classes=bool(compiles) and all(built.get(c, False) for c in compiles),
            kotlinc=counts(kotlinc),
            krusty=dict(passed_kotlinc=len(passing), passed=compared['passed'], failed=compared['failed'],
                        skipped=compared['skipped'], not_run=compared['not_run']),
            not_passing=lost[:EXAMPLES],
        ))
    return dict(exit=exits, total=totals(tasks),
                krusty_classes=totals([t for t in tasks if t['krusty_classes']]), tasks=tasks)
