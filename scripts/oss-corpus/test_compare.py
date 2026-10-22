#!/usr/bin/env python3
"""compare.py over a Kotlin Multiplatform JVM target: its main and test compilations write
`build/classes/kotlin/jvm/main` and `.../jvm/test` and are reported as two compilations, and a
fallback marks only the compilation of the task it names."""
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
            write(f'{work}/logs/kmp.compilations.tsv',
                  b':core:compileKotlinJvm\tcore/build/classes/kotlin/jvm/main\n'
                  b':core:compileTestKotlinJvm\tcore/build/classes/kotlin/jvm/test\n')
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
                 fallback=None, classes=2, identical=1, only_kotlinc=0, only_krusty=0),
            dict(task=':core:compileTestKotlinJvm', dir='core/build/classes/kotlin/jvm/test',
                 built=False, fallback='unsupported plugin', classes=1, identical=0,
                 only_kotlinc=None, only_krusty=None),
        ])


if __name__ == '__main__':
    unittest.main()
