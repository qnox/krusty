#!/usr/bin/env python3
"""summarize.py: the per-project table with stars, the reasons for each missed compilation, and the
plugins the corpus uses, counted once per project."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def project(name, rows, reference_exit=0):
    return dict(name=name, reference_exit=reference_exit, loc=dict(kotlin=1200, java=30),
                compilations=len(rows), built=sum(r['built'] for r in rows),
                classes=sum(r['classes'] for r in rows), identical=sum(r['identical'] for r in rows),
                rows=rows, no_reference_classes=[])


def row(task, built, classes, identical, fallback=None, error=None):
    return dict(task=task, dir='d', built=built, fallback=fallback, error=error, classes=classes,
                identical=identical, only_kotlinc=None, only_krusty=None)


class Summary(unittest.TestCase):
    def test_a_published_result_shows_the_run_that_measured_it(self):
        with tempfile.TemporaryDirectory() as work:
            p = project('zeta', [row(':a:compileKotlin', True, 4, 4)])
            p['measured'] = dict(date='2026-10-25', commit='4dd74752cb99f80c', run='https://example.test/run/1')
            with open(f'{work}/zeta.json', 'w') as f:
                json.dump(p, f)
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'), f'{work}/zeta.json'],
                                  check=True, capture_output=True, text=True).stdout
        self.assertEqual(text.splitlines()[:5], [
            '| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Refused up front | Unsupported | Internal error | Reported errors | Failed | Not run | Identical classes | Measured |',
            '| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |',
            '| zeta | not fetched | 1,200 | 30 | ok | 1 / 1 | 0 | 0 | 0 | 0 | 0 | 0 | 4 / 4 (100.0%) | [2026-10-25, krusty 4dd7475](https://example.test/run/1) |',
            '',
            '**Measured total:** 1 / 1 compilations built by krusty, 4 / 4 identical classes (100.0%).',
        ])

    def test_the_scoreboard_splits_compilations_by_outcome(self):
        blocked = row(':d:compileKotlin', False, 1, 0)
        blocked['blocked_by'] = [':e:compileKotlin']
        with tempfile.TemporaryDirectory() as work:
            p = project('zeta', [row(':a:compileKotlin', True, 4, 4),
                                 row(':b:compileKotlin', True, 1, 1),
                                 row(':c:compileKotlin', False, 2, 0, fallback='krusty does not implement '
                                     'everything it requests (krusty: unsupported compiler plugin '
                                     "'plugin id 'androidx.compose'': no)"),
                                 row(':g:compileKotlin', False, 2, 0,
                                     fallback='krusty does not implement everything the module uses'),
                                 row(':h:compileKotlin', False, 2, 0,
                                     fallback='krusty failed with an internal error'),
                                 row(':i:compileKotlin', False, 2, 0, fallback='krusty reported errors'),
                                 row(':j:compileKotlin', False, 2, 0, fallback='krusty reported errors'),
                                 row(':e:compileKotlin', False, 3, 0, error='e: boom'),
                                 row(':f:compileKotlin', False, 3, 0, error='e: boom'),
                                 blocked])
            with open(f'{work}/zeta.json', 'w') as f:
                json.dump(p, f)
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'), f'{work}/zeta.json'],
                                  check=True, capture_output=True, text=True).stdout
        self.assertEqual(text.splitlines()[:3], [
            '| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | '
            'Refused up front | Unsupported | Internal error | Reported errors | Failed | Not run | Identical classes |',
            '| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |',
            '| zeta | not fetched | 1,200 | 30 | ok | 2 / 10 | 1 | 1 | 1 | 2 | 2 | 1 | 5 / 22 (22.7%) |',
        ])

    def test_report_and_json(self):
        with tempfile.TemporaryDirectory() as work:
            reports = []
            for p in (
                project('zeta', [row(':a:compileKotlin', True, 4, 3),
                                 row(':b:compileKotlin', False, 2, 0, fallback=(
                                     "krusty does not implement everything it requests (krusty does not "
                                     "implement the kotlinc argument '-Xfoo'; krusty: unsupported compiler "
                                     "plugin '/c/kotlin-compose-compiler-plugin-embeddable-2.4.21.jar': no; "
                                     "krusty: unsupported compiler plugin 'plugin id 'androidx.compose'': no)")),
                                 row(':c:compileKotlin', False, 1, 0, fallback='compose')]),
                project('Alpha', [row(':compileKotlin', False, 5, 0, error='e: A.kt:1:1 boom')],
                        reference_exit=1),
            ):
                path = os.path.join(work, p['name'] + '.json')
                with open(path, 'w') as f:
                    json.dump(p, f)
                reports.append(path)
            with open(f'{work}/stars.tsv', 'w') as f:
                f.write('zeta\t12345\nAlpha\t7\n')
            os.makedirs(f'{work}/logs/x')
            with open(f'{work}/logs/x/zeta.plugins.tsv', 'w') as f:
                f.write('gradle\t:a\torg.jetbrains.kotlin.jvm\ngradle\t:b\torg.jetbrains.kotlin.jvm\n'
                        'gradle\t:a\torg.gradle.java\n'
                        'gradle\t:a\torg.jetbrains.kotlin.gradle.plugin.BuildMetricsPlugin\n'
                        'compiler\t:b\tkotlin-scripting-jvm-2.4.20.jar\n'
                        'compiler\t:b\tkotlin-stdlib-2.4.20.jar\n'
                        'compiler\t:b\tkotlin-compose-compiler-plugin-embeddable-2.4.20.jar\n')
            with open(f'{work}/logs/Alpha.plugins.tsv', 'w') as f:
                f.write('gradle\t:\torg.jetbrains.kotlin.jvm\n')
            text = subprocess.run(
                [sys.executable, os.path.join(HERE, 'summarize.py'), '--stars', f'{work}/stars.tsv',
                 '--plugins', f'{work}/logs', '--json', f'{work}/out.json', *reports],
                check=True, capture_output=True, text=True).stdout
            with open(f'{work}/out.json') as f:
                document = json.load(f)
        self.assertEqual(text, '''\
| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Refused up front | Unsupported | Internal error | Reported errors | Failed | Not run | Identical classes |
| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Alpha | 7 | 1,200 | 30 | failed (exit 1) | 0 / 1 | 0 | 0 | 0 | 0 | 1 | 0 | 0 / 5 (0.0%) |
| zeta | 12,345 | 1,200 | 30 | ok | 1 / 3 | 2 | 0 | 0 | 0 | 0 | 0 | 3 / 7 (42.9%) |

**Measured total:** 1 / 3 compilations built by krusty, 3 / 7 identical classes (42.9%).

## Missed compilations

- **Alpha**
  - 1 × e: A.kt:1:1 boom
- **zeta**
  - 1 × left to kotlinc: compiler plugin androidx.compose, argument -Xfoo
  - 1 × left to kotlinc: compose

## Gradle plugins

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `org.jetbrains.kotlin.jvm` | 2 | 3 |

## Kotlin compiler plugins

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `kotlin-compose-compiler-plugin-embeddable` | 1 | 1 |
''')
        self.assertEqual([(p['name'], p['stars'], p['misses']) for p in document['projects']], [
            ('Alpha', 7, [['e: A.kt:1:1 boom', 1]]),
            ('zeta', 12345, [['left to kotlinc: compiler plugin androidx.compose, argument -Xfoo', 1],
                             ['left to kotlinc: compose', 1]]),
        ])

    def test_root_failures_are_ranked_by_the_compilations_they_kept_from_running(self):
        with tempfile.TemporaryDirectory() as work:
            not_run = 'not run: a task it depends on failed'
            zeta = project('zeta', [row(':a:compileKotlin', False, 1, 0, error='e: a/A.kt:1:1 boom'),
                                    row(':c:compileKotlin', False, 1, 0, error=not_run),
                                    row(':d:compileKotlin', False, 1, 0, error=not_run),
                                    row(':e:compileKotlin', False, 1, 0, error=not_run)])
            for r, roots in zip(zeta['rows'], ([], [':a:compileKotlin', ':k:kspKotlin'],
                                               [':a:compileKotlin', ':k:kspKotlin'], [':k:kspKotlin'])):
                r['blocked_by'] = roots
            zeta['failed_tasks'] = {':a:compileKotlin': 'e: a/A.kt:1:1 boom', ':k:kspKotlin': None,
                                    ':z:compileKotlin': 'e: z/Z.kt:1:1 alone'}
            alpha = project('Alpha', [row(':x:compileKotlin', False, 1, 0, error='e: X.kt:2:2 bad'),
                                      row(':y:compileKotlin', False, 1, 0, error=not_run)])
            alpha['rows'][1]['blocked_by'] = [':x:compileKotlin']
            alpha['failed_tasks'] = {':x:compileKotlin': 'e: X.kt:2:2 bad'}
            # A result recorded before the corpus traced root failures has neither field.
            old = project('old', [row(':o:compileKotlin', False, 1, 0, error=not_run)])
            reports = []
            for p in (zeta, alpha, old):
                reports.append(os.path.join(work, p['name'] + '.json'))
                with open(reports[-1], 'w') as f:
                    json.dump(p, f)
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'),
                                   '--json', f'{work}/summary.json'] + reports,
                                  check=True, capture_output=True, text=True).stdout
            with open(f'{work}/summary.json') as f:
                summary = {p['name']: p for p in json.load(f)['projects']}
        lines = text.splitlines()
        start = lines.index('## Root failures')
        self.assertEqual(lines[start:lines.index('## Missed compilations')], [
            '## Root failures',
            '',
            'Each failed task of the krusty builds, ranked by the compilations that did not run because it failed:',
            'fixing it lets them run. A compilation stopped by two failed tasks counts for each.',
            '',
            '| Project | Failed task | First error | Compilations it kept from running |',
            '| --- | --- | --- | ---: |',
            '| zeta | `:k:kspKotlin` | no error recorded | 3 |',
            '| zeta | `:a:compileKotlin` | `e: a/A.kt:1:1 boom` | 2 |',
            '| Alpha | `:x:compileKotlin` | `e: X.kt:2:2 bad` | 1 |',
            '',
        ])
        self.assertEqual(summary['zeta']['roots'], [
            dict(task=':k:kspKotlin', error=None, blocked=3),
            dict(task=':a:compileKotlin', error='e: a/A.kt:1:1 boom', blocked=2),
            dict(task=':z:compileKotlin', error='e: z/Z.kt:1:1 alone', blocked=0),
        ])
        self.assertIsNone(summary['old']['roots'])

    def test_changes_since_the_previous_run_are_listed_per_project(self):
        with tempfile.TemporaryDirectory() as work:
            before = [project('zeta', [row(':a:compileKotlin', True, 4, 3),
                                       row(':b:compileKotlin', True, 2, 2),
                                       row(':c:compileKotlin', False, 1, 0, error='e: C.kt:1:1 old')]),
                      project('same', [row(':s:compileKotlin', True, 1, 1)]),
                      project('gone', [row(':g:compileKotlin', True, 1, 1)])]
            with open(f'{work}/previous.json', 'w') as f:
                json.dump(dict(projects=before), f)
            now = [project('zeta', [row(':a:compileKotlin', True, 4, 4),
                                    row(':b:compileKotlin', False, 2, 0, error='e: B.kt:1:1 new'),
                                    row(':c:compileKotlin', True, 1, 1),
                                    row(':d:compileKotlin', True, 1, 1)]),
                   project('same', [row(':s:compileKotlin', True, 1, 1)]),
                   project('fresh', [row(':f:compileKotlin', True, 1, 0)])]
            reports = []
            for p in now:
                reports.append(os.path.join(work, p['name'] + '.json'))
                with open(reports[-1], 'w') as f:
                    json.dump(p, f)
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'),
                                   '--previous', f'{work}/previous.json'] + reports,
                                  check=True, capture_output=True, text=True).stdout
        lines = text.splitlines()
        start = lines.index('## Since the previous run')
        self.assertEqual(lines[start:lines.index('## Missed compilations')], [
            '## Since the previous run',
            '',
            '| Project | Compilations built by krusty | Identical classes |',
            '| --- | ---: | ---: |',
            '| fresh | new: 1 / 1 | new: 0 / 1 |',
            '| gone | not measured (was 1 / 1) | not measured (was 1 / 1) |',
            '| zeta | 2 → 3 (+1) / 4 | 5 → 6 (+1) / 8 |',
            '',
            'No longer built by krusty:',
            '',
            '- **zeta** `:b:compileKotlin`: `e: B.kt:1:1 new`',
            '',
            'Newly built by krusty:',
            '',
            '- **zeta** `:c:compileKotlin`, `:d:compileKotlin`',
            '',
        ])

    def test_the_job_summary_drops_the_largest_error_listings_to_fit_its_size_limit(self):
        def verified(task, count):
            r = row(task, False, 1, 0, error='not run: a task it depends on failed')
            r['verify'] = dict(status='failed', errors=[f"{task}/F{n}.kt:1:1: error: unresolved reference 'x{n}'."
                                                        for n in range(count)])
            return r

        def compiled(task):
            return dict(row(task, True, 1, 1), verify=dict(status='compiled', errors=[]))
        with tempfile.TemporaryDirectory() as work:
            reports = []
            for name, count in (('big', 400), ('small', 2)):
                p = dict(project(name, [verified(':compileKotlin', count), compiled(':other:compileKotlin')]), verify_exit=1)
                path = os.path.join(work, name + '.json')
                with open(path, 'w') as f:
                    json.dump(p, f)
                reports.append(path)
            command = [sys.executable, os.path.join(HERE, 'summarize.py'), '--step-summary', f'{work}/job.md']
            full = subprocess.run([*command, *reports], check=True, capture_output=True, text=True).stdout
            with open(f'{work}/job.md') as f:
                unlimited = f.read()
            os.remove(f'{work}/job.md')
            subprocess.run([*command, '--step-summary-limit', '6000', *reports], check=True,
                           capture_output=True, text=True)
            with open(f'{work}/job.md') as f:
                trimmed = f.read()
        self.assertEqual(unlimited, full)
        self.assertGreater(len(full.encode()), 6000)
        self.assertLessEqual(len(trimmed.encode()), 6000)
        lines = trimmed.splitlines()
        self.assertIn('<details><summary><b>small</b>: 2 errors in 1 compilation, 1 cause; '
                      '2 of 2 compilations verified (verify build exit 1)</summary>', lines)
        self.assertEqual([line for line in lines if line.startswith('- **big**: ')], [
            '- **big**: 400 errors in 1 compilation, 1 cause; 2 of 2 compilations verified (verify build exit 1); '
            'listed in summary.md of the summary artifact'])
        self.assertEqual(trimmed.count('<details>'), trimmed.count('</details>'))

    def test_verify_errors_are_listed_per_project_and_grouped_by_cause(self):
        def verified(task, *errors, status='failed'):
            r = row(task, False, 1, 0, error='not run: a task it depends on failed')
            r['verify'] = dict(status=status, errors=list(errors))
            return r
        with tempfile.TemporaryDirectory() as work:
            reports = []
            for p in (
                dict(project('zeta', [
                    verified(':a:compileKotlin', "a/A.kt:3:5: error: unresolved reference 'foo'.",
                             "a/A.kt:9:1: error: unresolved reference 'bar'.",
                             'a/A.kt:12:1: error: type mismatch: inferred type is List<T> but `Map|K` was expected'),
                    verified(':b:compileKotlin', "b/B.kt:1:1: error: unresolved reference 'foo'."),
                    verified(':c:compileKotlin', status='compiled'),
                    dict(row(':d:compileKotlin', False, 1, 0, fallback='plugin'), verify=None),
                ]), verify_exit=1),
                dict(project('Alpha', [verified(':compileKotlin', "krusty: error: unresolved reference 'x'.",
                                                "error: doesn't support 'y' at 4")]), verify_exit=0),
                # A result published before the corpus ran the verify build has no verify_exit and
                # no entry.
                project('beta', [row(':compileKotlin', True, 1, 1)]),
                # A verify build that failed before writing any record still has an entry.
                dict(project('gamma', [dict(row(':compileKotlin', False, 1, 0, error='boom'), verify=None)]),
                     verify_exit=1),
            ):
                path = os.path.join(work, p['name'] + '.json')
                with open(path, 'w') as f:
                    json.dump(p, f)
                reports.append(path)
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'),
                                   '--json', f'{work}/out.json', *reports],
                                  check=True, capture_output=True, text=True).stdout
            with open(f'{work}/out.json') as f:
                document = json.load(f)
        section = text[text.index('## krusty errors'):].splitlines()
        self.assertEqual(section[8:], [
            '| Cause | Projects | Compilations | Errors |',
            '| --- | ---: | ---: | ---: |',
            "| `unresolved reference '…'.` | 2 | 3 | 4 |",
            "| `doesn't support 'y' at N` | 1 | 1 | 1 |",
            '| ``type mismatch: inferred type is List<T> but `…` was expected`` | 1 | 1 | 1 |',
            '',
            '<details><summary><b>Alpha</b>: 2 errors in 1 compilation, 2 causes; 1 of 1 compilation verified</summary>',
            '',
            "- `doesn't support 'y' at N`",
            "  - `error: doesn't support 'y' at 4` in `:compileKotlin`",
            "- `unresolved reference '…'.`",
            "  - `krusty: error: unresolved reference 'x'.` in `:compileKotlin`",
            '',
            '</details>',
            '',
            '- **gamma**: no compilation verified (verify build exit 1)',
            '',
            '<details><summary><b>zeta</b>: 4 errors in 2 compilations, 2 causes; 3 of 4 compilations verified '
            '(verify build exit 1)</summary>',
            '',
            "- `unresolved reference '…'.`",
            "  - `a/A.kt:3:5: error: unresolved reference 'foo'.` in `:a:compileKotlin`",
            "  - `a/A.kt:9:1: error: unresolved reference 'bar'.` in `:a:compileKotlin`",
            "  - `b/B.kt:1:1: error: unresolved reference 'foo'.` in `:b:compileKotlin`",
            '- ``type mismatch: inferred type is List<T> but `…` was expected``',
            '  - ``a/A.kt:12:1: error: type mismatch: inferred type is List<T> but `Map|K` was expected`` in `:a:compileKotlin`',
            '',
            '</details>',
        ])
        self.assertEqual([(p['name'], p['verification']) for p in document['projects']], [
            ('Alpha', dict(exit=0, verified=1, unverified=0)),
            ('beta', None),
            ('gamma', dict(exit=1, verified=0, unverified=1)),
            ('zeta', dict(exit=1, verified=3, unverified=1)),
        ])
        self.assertEqual([(p['name'], p['errors']) for p in document['projects']], [
            ('Alpha', [
                dict(cause="doesn't support 'y' at N", errors=[
                    dict(error="error: doesn't support 'y' at 4", compilations=[':compileKotlin'])]),
                dict(cause="unresolved reference '…'.", errors=[
                    dict(error="krusty: error: unresolved reference 'x'.", compilations=[':compileKotlin'])]),
            ]),
            ('beta', []),
            ('gamma', []),
            ('zeta', [
                dict(cause="unresolved reference '…'.", errors=[
                    dict(error="a/A.kt:3:5: error: unresolved reference 'foo'.", compilations=[':a:compileKotlin']),
                    dict(error="a/A.kt:9:1: error: unresolved reference 'bar'.", compilations=[':a:compileKotlin']),
                    dict(error="b/B.kt:1:1: error: unresolved reference 'foo'.", compilations=[':b:compileKotlin'])]),
                dict(cause='type mismatch: inferred type is List<T> but `…` was expected', errors=[
                    dict(error='a/A.kt:12:1: error: type mismatch: inferred type is List<T> but `Map|K` was expected',
                         compilations=[':a:compileKotlin'])]),
            ]),
        ])

    def test_a_verify_build_that_recorded_nothing_is_reported_not_omitted(self):
        with tempfile.TemporaryDirectory() as work:
            p = dict(project('zeta', [row(':a:compileKotlin', False, 2, 0,
                                          error='not run: a task it depends on failed')]),
                     verify_exit=1)
            p['rows'][0]['verify'] = None
            with open(f'{work}/zeta.json', 'w') as f:
                json.dump(p, f)
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'),
                                   '--json', f'{work}/out.json', f'{work}/zeta.json'],
                                  check=True, capture_output=True, text=True).stdout
            with open(f'{work}/out.json') as f:
                document = json.load(f)
        self.assertEqual(text, '''\
| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Refused up front | Unsupported | Internal error | Reported errors | Failed | Not run | Identical classes |
| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| zeta | not fetched | 1,200 | 30 | ok | 0 / 1 | 0 | 0 | 0 | 0 | 1 | 0 | 0 / 2 (0.0%) |

**Measured total:** 0 / 1 compilations built by krusty, 0 / 2 identical classes (0.0%).

## Missed compilations

- **zeta**
  - 1 × not run: a task it depends on failed

## krusty errors

The verify build compiles every compilation with kotlinc, and krusty compiles each one on its own
beside it, against kotlinc's classes of the compilations it depends on (the krusty plugin's
verify mode). One failure therefore never hides the next: these are all the errors krusty
reported, grouped by cause, the message without its location and with quoted text and numbers
masked. A result measured before the corpus ran the verify build is left out.

No verified compilation reported an error.

- **zeta**: no compilation verified (verify build exit 1)
''')
        self.assertEqual([(p['name'], p['misses'], p['errors'], p['verification']) for p in document['projects']], [
            ('zeta', [['not run: a task it depends on failed', 1]], [], dict(exit=1, verified=0, unverified=1)),
        ])

    def test_a_project_without_a_result_is_listed_as_not_measured_with_its_reason(self):
        with tempfile.TemporaryDirectory() as work:
            def log(name, status, build, text):
                os.makedirs(f'{work}/results/corpus-{name}/logs')
                with open(f'{work}/results/corpus-{name}/logs/STATUS', 'w') as f:
                    f.write(status)
                if text is not None:
                    with open(f'{work}/results/corpus-{name}/logs/{name}.{build}.log', 'w') as f:
                        f.write(text)
            log('okio', 'okio warmup exit 1 seconds 22\nokio kotlinc exit 1 seconds 4\n', 'warmup',
                'Welcome to Gradle 9.8.1!\n\n* What went wrong:\nA problem occurred configuring project \':build-support\'.\n'
                '> Could not resolve all artifacts for configuration \'classpath\'.\n'
                '      > Received status code 429 from server: Too Many Requests\n')
            log('koin', 'koin warmup exit 0 seconds 20\nkoin kotlinc exit 2 seconds 5\n', 'kotlinc', 'no cause line here\n')
            log('moshi', 'moshi warmup exit 3 seconds 2\n', 'warmup', None)
            # An interrupted project: its builds passed and its reference test build failed.
            log('ktor', 'ktor warmup exit 0 seconds 9\nktor kotlinc exit 0 seconds 8\n'
                        'ktor kotlinc tests exit 1 seconds 7\n', 'kotlinc.tests',
                '* What went wrong:\nExecution failed for task \':test\'.\n')
            log('Unciv', 'Unciv warmup exit 0 seconds 2\nUnciv kotlinc exit 0 seconds 2\n', 'kotlinc', '')
            with open(f'{work}/zeta.json', 'w') as f:
                json.dump(project('zeta', [row(':a:compileKotlin', True, 4, 4)]), f)
            with open(f'{work}/stars.tsv', 'w') as f:
                f.write('zeta\t5\nokio\t\nkoin\t9\nghost\t\nmoshi\t3\nUnciv\t4\nktor\t\n')
            text = subprocess.run(
                [sys.executable, os.path.join(HERE, 'summarize.py'), '--expected', f'{work}/stars.tsv',
                 '--results', f'{work}/results', '--json', f'{work}/out.json', f'{work}/zeta.json'],
                check=True, capture_output=True, text=True).stdout
            with open(f'{work}/out.json') as f:
                document = json.load(f)
        lines = text.splitlines()
        self.assertEqual(lines[lines.index('## Not measured'):lines.index('## Missed compilations')], [
            '## Not measured',
            '',
            'These projects are in the corpus but produced no comparison in this run, so they have no row above:',
            '',
            '| Project | Why |',
            '| --- | --- |',
            '| ghost | its job uploaded no logs: it stopped before a build ran, or the artifact is missing |',
            '| koin | the kotlinc build exited 2 after 5 s |',
            "| ktor | the kotlinc tests build exited 1 after 7 s: Execution failed for task ':test'. |",
            '| moshi | the warmup build exited 3 after 2 s and left no log |',
            "| okio | the warmup build exited 1 after 22 s: A problem occurred configuring project ':build-support'."
            ' (a download answered HTTP 429) |',
            '| Unciv | every build exited 0 but the job wrote no comparison |',
            '',
        ])
        self.assertEqual(document['not_measured'], [
            dict(name='ghost', reason='its job uploaded no logs: it stopped before a build ran, or the artifact is missing'),
            dict(name='koin', reason='the kotlinc build exited 2 after 5 s'),
            dict(name='ktor', reason="the kotlinc tests build exited 1 after 7 s: Execution failed for task ':test'."),
            dict(name='moshi', reason='the warmup build exited 3 after 2 s and left no log'),
            dict(name='okio', reason="the warmup build exited 1 after 22 s: A problem occurred configuring project "
                                     "':build-support'. (a download answered HTTP 429)"),
            dict(name='Unciv', reason='every build exited 0 but the job wrote no comparison'),
        ])

    def test_a_complete_run_has_no_not_measured_section(self):
        with tempfile.TemporaryDirectory() as work:
            with open(f'{work}/zeta.json', 'w') as f:
                json.dump(project('zeta', [row(':a:compileKotlin', True, 4, 4)]), f)
            with open(f'{work}/stars.tsv', 'w') as f:
                f.write('zeta\t5\n')
            text = subprocess.run(
                [sys.executable, os.path.join(HERE, 'summarize.py'), '--expected', f'{work}/stars.tsv',
                 '--results', f'{work}/results', '--json', f'{work}/out.json', f'{work}/zeta.json'],
                check=True, capture_output=True, text=True).stdout
            with open(f'{work}/out.json') as f:
                document = json.load(f)
        self.assertNotIn('## Not measured', text)
        self.assertEqual(document['not_measured'], [])

    def test_no_table_has_an_empty_cell(self):
        def cost(rss):
            return dict(built_compile_seconds=1.0, compile_seconds=2.0, wall_seconds=3.0, cpu_seconds=4.0,
                        peak_rss_mb=dict(total=rss, gradle=0, kotlin_daemon=rss, krusty=rss, other=0))
        with tempfile.TemporaryDirectory() as work:
            p = project('zeta', [row(':a:compileKotlin', False, 4, 0, error='e: boom')])
            p['failed_tasks'] = {':a:compileKotlin': None}
            p['rows'][0]['blocked_by'] = [':a:compileKotlin']
            p['cost'] = dict(kotlinc=cost(100), krusty=cost(0))
            p['tests'] = dict(exit=dict(kotlinc=0, krusty=1),
                              unavailable=dict(kotlinc=dict(inventory='missing')))
            with open(f'{work}/zeta.json', 'w') as f:
                json.dump(p, f)
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'), f'{work}/zeta.json'],
                                  check=True, capture_output=True, text=True).stdout
        blank = [line for line in text.splitlines()
                 if line.startswith('|') and any(cell.strip() == '' for cell in line.strip('|').split('|'))]
        self.assertEqual(blank, [])

    def test_build_cost_compares_both_builds_of_each_measured_project(self):
        def cost(built, compile_, wall, cpu, daemon, krusty, total):
            return dict(built_compile_seconds=built, compile_seconds=compile_, wall_seconds=wall,
                        cpu_seconds=cpu, peak_rss_mb=dict(total=total, gradle=0, kotlin_daemon=daemon,
                                                          krusty=krusty, other=0))
        with tempfile.TemporaryDirectory() as work:
            reports = []
            measured = project('zeta', [row(':a:compileKotlin', True, 4, 4)])
            measured['cost'] = dict(kotlinc=cost(30.0, 40.0, 120.0, 300.0, 1500, 0, 2500),
                                    krusty=cost(10.0, 25.0, 90.0, 200.0, 0, 400, 1400))
            # A result published before builds were measured has no cost and no row.
            earlier = project('Alpha', [row(':compileKotlin', True, 1, 1)])
            for p in (measured, earlier):
                path = os.path.join(work, p['name'] + '.json')
                with open(path, 'w') as f:
                    json.dump(p, f)
                reports.append(path)
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'), *reports],
                                  check=True, capture_output=True, text=True).stdout
        section = text[text.index('## Build cost'):].splitlines()
        self.assertEqual(section[-5:], [
            '| Project | Compile time, compilations krusty built (kotlinc / krusty) | Compile time, all compilations |'
            ' Compiler peak RSS | Build peak RSS | Build wall time | Build CPU time |',
            '| --- | ---: | ---: | ---: | ---: | ---: | ---: |',
            '| zeta | 30.0 s / 10.0 s (3.0×) | 40.0 s / 25.0 s | 1,500 MB / 400 MB | 2,500 MB / 1,400 MB | '
            '120.0 s / 90.0 s | 300.0 s / 200.0 s |',
            '',
            '**Total** over 1 project: compile time of the compilations krusty built 30.0 s / 10.0 s (3.0×), '
            'of all compilations 40.0 s / 25.0 s, build wall time 120.0 s / 90.0 s, build CPU time 300.0 s / 200.0 s.',
        ])

    def test_hotspots_rank_functions_across_the_sampled_projects(self):
        with tempfile.TemporaryDirectory() as work:
            reports = []
            for name, functions, graph in (
                ('zeta', [dict(function='zip::find', self=60, total=60),
                          dict(function='krusty::resolve', self=20, total=70)], True),
                ('Alpha', [dict(function='krusty::resolve', self=15, total=20)], False),
            ):
                p = project(name, [row(':a:compileKotlin', True, 1, 1)])
                p['hotspots'] = dict(samples=100, functions=functions)
                if graph:
                    p['flame_graph'] = 'krusty-flamegraph.svg'
                path = os.path.join(work, name + '.json')
                with open(path, 'w') as f:
                    json.dump(p, f)
                reports.append(path)
            text = subprocess.run([sys.executable, os.path.join(HERE, 'summarize.py'), *reports],
                                  check=True, capture_output=True, text=True).stdout
        self.assertEqual(text[text.index('## krusty hotspots'):].splitlines(), [
            '## krusty hotspots',
            '',
            'Sampled call stacks of the krusty processes of every krusty build (200 samples at 99 Hz).',
            '*Self* counts the samples in the function itself, *total* those with it anywhere on the stack.',
            '',
            'Flame graphs: [zeta](projects/zeta/krusty-flamegraph.svg).',
            '',
            '| Function | Self | Total | Projects |',
            '| --- | ---: | ---: | --- |',
            '| `zip::find` | 30.0% | 30.0% | zeta |',
            '| `krusty::resolve` | 17.5% | 45.0% | Alpha, zeta |',
        ])


if __name__ == '__main__':
    unittest.main()
