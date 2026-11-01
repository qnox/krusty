#!/usr/bin/env python3
"""The projects' own tests run on kotlinc's and on krusty's classes: `compare.py` compares only the
tests that passed on kotlinc's classes, separates the test tasks that run classes krusty built only,
`run.sh` moves each test build's JUnit reports out of the checkout, and `summarize.py` reports it."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'w') as f:
        f.write(data)


def suite(cls, cases):
    """A JUnit XML report of one test class: {test: 'passed' | 'failed' | 'error' | 'skipped'}."""
    body = {'passed': '', 'failed': '<failure message="x"/>', 'error': '<error message="x"/>',
            'skipped': '<skipped/>'}
    return (f'<?xml version="1.0" encoding="UTF-8"?>\n<testsuite name="{cls}" tests="{len(cases)}">\n'
            + ''.join(f'  <testcase name="{n}" classname="{cls}" time="0.1">{body[o]}</testcase>\n'
                      for n, o in cases.items())
            + '</testsuite>\n')


class Compare(unittest.TestCase):
    def compare(self, work):
        out = f'{work}/out/p'
        for mode in ('kotlinc', 'krusty'):
            write(f'{out}/{mode}/lib/build/classes/kotlin/main/l/L.class', 'L')
            write(f'{out}/{mode}/lib/build/classes/kotlin/test/l/LT.class', 'LT')
            write(f'{out}/{mode}/app/build/classes/kotlin/main/a/A.class', 'A')
        write(f'{work}/logs/p.kotlinc.compilations.tsv',
              ':app:compileKotlin\tapp/build/classes/kotlin/main\n'
              ':lib:compileKotlin\tlib/build/classes/kotlin/main\n'
              ':lib:compileTestKotlin\tlib/build/classes/kotlin/test\n')
        # The app's compile is left to kotlinc, so its test task does not run krusty's classes only.
        write(f'{work}/logs/p.krusty.log',
              "krusty: task ':app:compileKotlin' is left to kotlinc: unsupported plugin\n")
        inventory = (':app:test\tapp/build/test-results/test\t:app:compileKotlin,:lib:compileKotlin\n'
                     ':lib:test\tlib/build/test-results/test\t:lib:compileKotlin,:lib:compileTestKotlin\n'
                     ':empty:test\tempty/build/test-results/test\t\n')
        write(f'{work}/logs/p.kotlinc.tests.tsv', inventory)
        write(f'{work}/logs/p.krusty.tests.tsv', inventory)
        # Neither test build changed the measured outputs.
        write(f'{work}/logs/p.kotlinc.changed.tsv', '')
        write(f'{work}/logs/p.krusty.changed.tsv', '')
        write(f'{out}/kotlinc-tests/lib/build/test-results/test/TEST-l.LT.xml',
              suite('l.LT', {'ok': 'passed', 'bad': 'failed', 'broken': 'passed', 'gone': 'passed',
                             'off': 'passed', 'never': 'skipped', 'reference': 'error'}))
        write(f'{out}/kotlinc-tests/app/build/test-results/test/TEST-a.AT.xml',
              suite('a.AT', {'ok': 'passed'}))
        # krusty's run fails `broken`, does not report `gone`, skips `off`, and passes the test the
        # reference fails, which is not compared.
        write(f'{out}/krusty-tests/lib/build/test-results/test/TEST-l.LT.xml',
              suite('l.LT', {'ok': 'passed', 'bad': 'passed', 'broken': 'error', 'off': 'skipped',
                             'never': 'skipped', 'reference': 'passed'}))
        write(f'{out}/krusty-tests/app/build/test-results/test/TEST-a.AT.xml',
              suite('a.AT', {'ok': 'passed'}))
        subprocess.run([sys.executable, os.path.join(HERE, 'compare.py'), work, 'p', '0', '0', '1'],
                       check=True, stdout=subprocess.DEVNULL)
        with open(f'{out}/compare.json') as f:
            return json.load(f)

    def test_only_tests_passing_on_kotlinc_classes_are_compared(self):
        with tempfile.TemporaryDirectory() as work:
            tests = self.compare(work)['tests']
        self.assertEqual(tests['exit'], dict(kotlinc=0, krusty=1))
        self.assertEqual(tests['tasks'], [
            dict(task=':app:test', compilations=[':app:compileKotlin', ':lib:compileKotlin'],
                 krusty_classes=False, kotlinc=dict(passed=1, failed=0, skipped=0),
                 krusty=dict(passed_kotlinc=1, passed=1, failed=0, skipped=0, not_run=0), not_passing=[]),
            dict(task=':lib:test', compilations=[':lib:compileKotlin', ':lib:compileTestKotlin'],
                 krusty_classes=True, kotlinc=dict(passed=4, failed=2, skipped=1),
                 krusty=dict(passed_kotlinc=4, passed=1, failed=1, skipped=1, not_run=1),
                 not_passing=['l.LT.broken', 'l.LT.gone', 'l.LT.off']),
            # A test task that runs no compared compilation never counts as running krusty's classes.
            dict(task=':empty:test', compilations=[], krusty_classes=False,
                 kotlinc=dict(passed=0, failed=0, skipped=0),
                 krusty=dict(passed_kotlinc=0, passed=0, failed=0, skipped=0, not_run=0), not_passing=[]),
        ])
        self.assertEqual(tests['total'], dict(tasks=3, passed_kotlinc=5, passed=2, failed=1, skipped=1, not_run=1))
        self.assertEqual(tests['krusty_classes'],
                         dict(tasks=1, passed_kotlinc=4, passed=1, failed=1, skipped=1, not_run=1))

    def test_a_task_the_krusty_test_build_never_listed_ran_none_of_its_tests(self):
        with tempfile.TemporaryDirectory() as work:
            self.compare(work)
            write(f'{work}/logs/p.krusty.tests.tsv', ':app:test\tapp/build/test-results/test\t:app:compileKotlin\n')
            subprocess.run([sys.executable, os.path.join(HERE, 'compare.py'), work, 'p', '0', '0', '1'],
                           check=True, stdout=subprocess.DEVNULL)
            with open(f'{work}/out/p/compare.json') as f:
                tests = json.load(f)['tests']
        lib = next(t for t in tests['tasks'] if t['task'] == ':lib:test')
        self.assertEqual(lib['krusty'], dict(passed_kotlinc=4, passed=0, failed=0, skipped=0, not_run=4))

    def test_without_test_builds_there_is_no_test_comparison(self):
        with tempfile.TemporaryDirectory() as work:
            write(f'{work}/out/p/kotlinc/a/build/classes/kotlin/main/A.class', 'A')
            write(f'{work}/logs/p.kotlinc.compilations.tsv', ':a:compileKotlin\ta/build/classes/kotlin/main\n')
            subprocess.run([sys.executable, os.path.join(HERE, 'compare.py'), work, 'p', '0'],
                           check=True, stdout=subprocess.DEVNULL)
            with open(f'{work}/out/p/compare.json') as f:
                self.assertIsNone(json.load(f)['tests'])

    def test_the_report_counts_tests_passing_on_krusty_classes(self):
        with tempfile.TemporaryDirectory() as work:
            project = self.compare(work)
            path = f'{work}/out/p/compare.json'
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'), path],
                                  check=True, capture_output=True, text=True).stdout
        self.assertIsNotNone(project['tests'])
        self.assertEqual(text[text.index('## Project tests'):].splitlines(), [
            '## Project tests',
            '',
            "Each project's own JVM tests run after each build, on the classes it compiled. Only a test that passed on",
            "kotlinc's classes is compared. A test task runs on *krusty's classes only* when krusty built every",
            'compilation it runs; the other tasks also run classes kotlinc compiled.',
            '',
            "| Project | Test builds (kotlinc / krusty exit) | Passing on krusty's classes, all tasks | Fail | Skipped |"
            " Not run | Passing, tasks on krusty's classes only |",
            '| --- | --- | ---: | ---: | ---: | ---: | ---: |',
            '| p | 0 / 1 | 2 / 5 (40.0%) | 1 | 1 | 1 | 1 / 4 (25.0%) in 1 / 3 tasks |',
            '| **Total** | | **2 / 5 (40.0%)** | **1** | **1** | **1** | **1 / 4 (25.0%) in 1 / 3 tasks** |',
            '',
            "First tests that pass on kotlinc's classes and not on krusty's:",
            '',
            '- **p** `:lib:test`: `l.LT.broken`, `l.LT.gone`, `l.LT.off`',
        ])


class RunScript(unittest.TestCase):
    """`run.sh` runs a test build after each measured build and moves its JUnit reports out of the
    checkout, so the krusty test build cannot read the kotlinc one's. The test build runs on the
    measured outputs, snapshotted before it starts; anything it changes in them is detected."""

    def run_corpus(self, work, gradlew):
        repo = f'{work}/upstream'
        write(f'{repo}/gradlew', gradlew)
        os.chmod(f'{repo}/gradlew', 0o755)
        git = ['git', '-C', repo, '-c', 'user.name=t', '-c', 'user.email=t@t']
        subprocess.run(['git', 'init', '-q', repo], check=True)
        subprocess.run(git + ['add', '.'], check=True)
        subprocess.run(git + ['commit', '-qm', 'init'], check=True)
        commit = subprocess.run(git + ['rev-parse', 'HEAD'], check=True, capture_output=True,
                                text=True).stdout.strip()
        write(f'{work}/projects.tsv', f'p\tfile://{repo}\t{commit}\t.\t21\t-\n')
        env = dict(os.environ, CORPUS_PROJECTS=f'{work}/projects.tsv')
        env.pop('CORPUS_PERF', None)
        result = subprocess.run(['bash', os.path.join(HERE, 'run.sh'), 'p', f'{work}/corpus',
                                 '/nonexistent/krusty', '/nonexistent/repository', '0'],
                                env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        with open(f'{work}/corpus/out/p/compare.json') as f:
            return json.load(f)

    def test_each_test_build_reports_on_its_own_classes(self):
        with tempfile.TemporaryDirectory() as work:
            # A build with one test task, whose test fails when the krusty plugin is applied.
            tests = self.run_corpus(work, '#!/bin/sh\n'
                                          'if [ -n "$CORPUS_COMPILATIONS" ]; then : > "$CORPUS_COMPILATIONS"; fi\n'
                                          'if [ -n "$CORPUS_TESTS" ]; then\n'
                                          '  printf ":test\\tbuild/test-results/test\\t\\n" > "$CORPUS_TESTS"\n'
                                          '  mkdir -p build/test-results/test\n'
                                          '  case "$*" in *krusty.binary*) body="<failure/>";; *) body="";; esac\n'
                                          '  echo "<testsuite><testcase classname=\\"T\\" name=\\"t\\">$body</testcase></testsuite>"'
                                          ' > build/test-results/test/TEST-T.xml\n'
                                          'fi\n')['tests']
            self.assertFalse(os.path.exists(f'{work}/corpus/src/p/build/test-results/test'))
        self.assertEqual(tests['exit'], dict(kotlinc=0, krusty=0))
        self.assertEqual(tests['total'], dict(tasks=1, passed_kotlinc=1, passed=0, failed=1, skipped=0, not_run=0))

    def test_a_test_build_that_rewrites_the_measured_classes_is_not_credited(self):
        with tempfile.TemporaryDirectory() as work:
            # The measured builds write different bytes; each test build then rewrites the class
            # to the same third bytes before its test passes.
            project = self.run_corpus(work, '#!/bin/sh\n'
                                            'case "$*" in *krusty.binary*) bytes=R;; *) bytes=K;; esac\n'
                                            'if [ -n "$CORPUS_COMPILATIONS" ]; then\n'
                                            '  printf ":compileKotlin\\tbuild/classes\\n" > "$CORPUS_COMPILATIONS"\n'
                                            '  mkdir -p build/classes && printf $bytes > build/classes/A.class\n'
                                            'fi\n'
                                            'if [ -n "$CORPUS_TESTS" ]; then\n'
                                            '  printf T > build/classes/A.class\n'
                                            '  printf ":test\\tbuild/test-results/test\\t:compileKotlin\\n" > "$CORPUS_TESTS"\n'
                                            '  mkdir -p build/test-results/test\n'
                                            '  echo "<testsuite><testcase classname=\\"T\\" name=\\"t\\"/></testsuite>"'
                                            ' > build/test-results/test/TEST-T.xml\n'
                                            'fi\n')
            measured = {}
            for mode in ('kotlinc', 'krusty'):
                with open(f'{work}/corpus/out/p/{mode}/build/classes/A.class') as f:
                    measured[mode] = f.read()
            self.assertFalse(os.path.exists(f'{work}/corpus/src/p/build/classes'))
            path = f'{work}/corpus/out/p/compare.json'
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'), path],
                                  check=True, capture_output=True, text=True).stdout
        # The snapshots and the byte comparison are the measured builds', not the test builds'.
        self.assertEqual(measured, dict(kotlinc='K', krusty='R'))
        self.assertEqual((project['built'], project['classes'], project['identical']), (1, 1, 0))
        self.assertEqual(project['tests'], dict(exit=dict(kotlinc=0, krusty=0), unavailable=dict(
            kotlinc=dict(changed=1, examples=['build/classes/A.class']),
            krusty=dict(changed=1, examples=['build/classes/A.class']))))
        # Nothing the test builds ran is counted.
        self.assertEqual(text[text.index('## Project tests'):text.index('## Build cost')].splitlines()[8:], [
            '| p | 0 / 0 | unavailable: the kotlinc test build changed 1 measured output files; '
            'the krusty test build changed 1 measured output files | | | | |',
            '| **Total** | | **0 / 0** | **0** | **0** | **0** | **0 / 0 in 0 / 0 tasks** |',
            '',
        ])

    def test_a_test_build_not_checked_against_the_measured_outputs_is_not_compared(self):
        with tempfile.TemporaryDirectory() as work:
            Compare().compare(work)
            os.remove(f'{work}/logs/p.krusty.changed.tsv')
            subprocess.run([sys.executable, os.path.join(HERE, 'compare.py'), work, 'p', '0', '0', '1'],
                           check=True, stdout=subprocess.DEVNULL)
            with open(f'{work}/out/p/compare.json') as f:
                tests = json.load(f)['tests']
        self.assertEqual(tests, dict(exit=dict(kotlinc=0, krusty=1),
                                     unavailable=dict(krusty=dict(changed=None, examples=[]))))


class Inventory(unittest.TestCase):
    """A reference test build that fails before writing its test inventory is reported, not
    dropped; an empty inventory is a build without test tasks; a run without test builds has no
    test record (`test_without_test_builds_there_is_no_test_comparison`)."""

    def test_a_reference_test_build_that_failed_before_listing_its_tasks_is_unavailable(self):
        with tempfile.TemporaryDirectory() as work:
            # The test builds fail during configuration: neither writes a test inventory.
            project = RunScript().run_corpus(work, '#!/bin/sh\n'
                                                   'if [ -n "$CORPUS_COMPILATIONS" ]; then : > "$CORPUS_COMPILATIONS"; fi\n'
                                                   'if [ -n "$CORPUS_TESTS" ]; then exit 1; fi\n')
            path = f'{work}/corpus/out/p/compare.json'
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'), path],
                                  check=True, capture_output=True, text=True).stdout
        self.assertEqual(project['tests'], dict(exit=dict(kotlinc=1, krusty=1),
                                                unavailable=dict(kotlinc=dict(inventory='missing'))))
        self.assertEqual(text[text.index('## Project tests'):text.index('## Build cost')].splitlines()[8:], [
            '| p | 1 / 1 | unavailable: the kotlinc test build listed no test task '
            '(it failed before Gradle planned its tasks) | | | | |',
            '| **Total** | | **0 / 0** | **0** | **0** | **0** | **0 / 0 in 0 / 0 tasks** |',
            '',
        ])

    def test_an_empty_inventory_is_a_build_without_test_tasks(self):
        with tempfile.TemporaryDirectory() as work:
            project = RunScript().run_corpus(work, '#!/bin/sh\n'
                                                   'if [ -n "$CORPUS_COMPILATIONS" ]; then : > "$CORPUS_COMPILATIONS"; fi\n'
                                                   'if [ -n "$CORPUS_TESTS" ]; then : > "$CORPUS_TESTS"; fi\n')
        self.assertEqual(project['tests'], dict(
            exit=dict(kotlinc=0, krusty=0), unavailable=None,
            total=dict(tasks=0, passed_kotlinc=0, passed=0, failed=0, skipped=0, not_run=0),
            krusty_classes=dict(tasks=0, passed_kotlinc=0, passed=0, failed=0, skipped=0, not_run=0),
            tasks=[]))


if __name__ == '__main__':
    unittest.main()
