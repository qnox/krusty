#!/usr/bin/env python3
"""compare.py over a Kotlin Multiplatform JVM target: its main and test compilations write
`build/classes/kotlin/jvm/main` and `.../jvm/test` and are reported as two compilations, and a
fallback marks only the compilation of the task it names. A listed task kotlinc wrote no class for
is reported apart."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'wb') as f:
        f.write(data)


class MultiplatformJvmTarget(unittest.TestCase):
    def test_main_and_test_compilations_stay_separate(self):
        with tempfile.TemporaryDirectory() as work:
            out = os.path.join(work, 'out', 'kmp')
            for mode in ('kotlinc', 'krusty'):
                write(f'{out}/{mode}/core/build/classes/kotlin/jvm/main/a/A.class', b'A')
                write(f'{out}/{mode}/core/build/classes/kotlin/jvm/main/a/B.class', b'B')
                write(f'{out}/{mode}/core/build/classes/kotlin/jvm/test/a/T.class', b'T')
            write(f'{out}/krusty/core/build/classes/kotlin/jvm/main/a/B.class', b'B from krusty')
            write(f'{work}/src/kmp/core/src/jvmMain/kotlin/A.kt', b'class A\n\nclass B\n')
            write(f'{out}/krusty/core/build/classes/kotlin/jvm/bench/a/Extra.class', b'E')
            write(f'{work}/logs/kmp.kotlinc.compilations.tsv',
                  b':core:compileKotlinJvm\tcore/build/classes/kotlin/jvm/main\n'
                  b':core:compileTestKotlinJvm\tcore/build/classes/kotlin/jvm/test\n'
                  b':core:compileBenchKotlinJvm\tcore/build/classes/kotlin/jvm/bench\n')
            write(f'{work}/logs/kmp.krusty.log',
                  b"krusty: task ':core:compileTestKotlinJvm' is left to kotlinc: unsupported plugin\n")
            subprocess.run([sys.executable, os.path.join(HERE, 'compare.py'), work, 'kmp', '0'],
                           check=True, stdout=subprocess.DEVNULL)
            with open(f'{out}/compare.json') as f:
                summary = json.load(f)
        self.assertEqual(summary['loc'], {'kotlin': 2, 'java': 0})
        self.assertEqual((summary['compilations'], summary['built']), (2, 1))
        self.assertEqual((summary['classes'], summary['identical']), (3, 1))
        self.assertEqual(summary['rows'], [
            dict(task=':core:compileKotlinJvm', dir='core/build/classes/kotlin/jvm/main', built=True,
                 fallback=None, error=None, classes=2, identical=1, only_kotlinc=0, only_krusty=0),
            dict(task=':core:compileTestKotlinJvm', dir='core/build/classes/kotlin/jvm/test',
                 built=False, fallback='unsupported plugin', error=None, classes=1, identical=0,
                 only_kotlinc=None, only_krusty=None),
        ])
        # A listed task kotlinc wrote no class for is reported apart, never dropped.
        self.assertEqual(summary['no_reference_classes'], [
            dict(task=':core:compileBenchKotlinJvm', dir='core/build/classes/kotlin/jvm/bench',
                 krusty_classes=1),
        ])



class FailedCompile(unittest.TestCase):
    def test_a_failed_compile_reports_the_first_error_under_its_replacement_task(self):
        with tempfile.TemporaryDirectory() as work:
            out = os.path.join(work, 'out', 'p')
            checkout = os.path.join(work, 'src', 'p')
            write(f'{out}/kotlinc/a/build/classes/kotlin/main/A.class', b'A')
            write(f'{out}/kotlinc/b/build/classes/kotlin/main/B.class', b'B')
            write(f'{out}/kotlinc/c/build/classes/kotlin/main/C.class', b'C')
            write(f'{work}/logs/p.kotlinc.compilations.tsv',
                  b':a:compileKotlin\ta/build/classes/kotlin/main\n'
                  b':b:compileKotlin\tb/build/classes/kotlin/main\n'
                  b':c:compileKotlin\tc/build/classes/kotlin/main\n')
            write(f'{work}/logs/p.krusty.log', (
                '> Task :a:compileKotlinWithKrusty\n'
                f'e: file://{checkout}/a/src/main/kotlin/A.kt:3:5 Unresolved reference: foo\n'
                f'e: file://{checkout}/a/src/main/kotlin/A.kt:4:5 Unresolved reference: bar\n'
                '> Task :a:compileKotlinWithKrusty FAILED\n'
                '> Task :b:compileKotlinWithKrusty\n').encode())
            subprocess.run([sys.executable, os.path.join(HERE, 'compare.py'), work, 'p', '0'],
                           check=True, stdout=subprocess.DEVNULL)
            with open(f'{out}/compare.json') as f:
                summary = json.load(f)
        self.assertEqual([(r['task'], r['built'], r['error']) for r in summary['rows']], [
            (':a:compileKotlin', False, 'e: a/src/main/kotlin/A.kt:3:5 Unresolved reference: foo'),
            (':b:compileKotlin', False, 'krusty wrote no class'),
            (':c:compileKotlin', False, 'not run: a task it depends on failed'),
        ])


if __name__ == '__main__':
    unittest.main()
