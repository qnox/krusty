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
                                 row(':b:compileKotlin', False, 2, 0, fallback='compose'),
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
  - 2 × left to kotlinc: compose

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
            ('zeta', 12345, [['left to kotlinc: compose', 2]]),
        ])


if __name__ == '__main__':
    unittest.main()
