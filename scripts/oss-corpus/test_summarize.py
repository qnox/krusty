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
            '| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes | Measured |',
            '| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |',
            '| zeta |  | 1,200 | 30 | ok | 1 / 1 | 4 / 4 (100.0%) | [2026-10-25, krusty 4dd7475](https://example.test/run/1) |',
            '| **Measured total** | | | | | **1 / 1** | **4 / 4 (100.0%)** | |',
            '',
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
| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes |
| --- | ---: | ---: | ---: | --- | ---: | ---: |
| Alpha | 7 | 1,200 | 30 | failed (exit 1) | 0 / 1 | 0 / 5 (0.0%) |
| zeta | 12,345 | 1,200 | 30 | ok | 1 / 3 | 3 / 7 (42.9%) |
| **Measured total** | | | | | **1 / 3** | **3 / 7 (42.9%)** |

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
| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes |
| --- | ---: | ---: | ---: | --- | ---: | ---: |
| zeta |  | 1,200 | 30 | ok | 0 / 1 | 0 / 2 (0.0%) |
| **Measured total** | | | | | **0 / 1** | **0 / 2 (0.0%)** |

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
        self.assertEqual(section[-4:], [
            '| Project | Compile time, compilations krusty built (kotlinc / krusty) | Compile time, all compilations |'
            ' Compiler peak RSS | Build peak RSS | Build wall time | Build CPU time |',
            '| --- | ---: | ---: | ---: | ---: | ---: | ---: |',
            '| zeta | 30.0 s / 10.0 s (3.0×) | 40.0 s / 25.0 s | 1,500 MB / 400 MB | 2,500 MB / 1,400 MB | '
            '120.0 s / 90.0 s | 300.0 s / 200.0 s |',
            '| **Total** | **30.0 s / 10.0 s (3.0×)** | **40.0 s / 25.0 s** | | | **120.0 s / 90.0 s** | '
            '**300.0 s / 200.0 s** |',
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
