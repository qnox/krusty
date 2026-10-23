#!/usr/bin/env python3
"""The corpus setup and output inventory: `outputs.py` snapshots each listed compile output wherever
the task writes it and clears it before the next build, `pin.py` rewrites exactly one Kotlin version
assignment, and `run.sh` stops before any build when a setup step fails."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import outputs  # noqa: E402
import pin  # noqa: E402


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'w') as f:
        f.write(data)


def read(path):
    with open(path) as f:
        return f.read()


class Outputs(unittest.TestCase):
    def test_a_nonstandard_destination_is_snapshotted_and_cleared_between_modes(self):
        with tempfile.TemporaryDirectory() as work:
            root = f'{work}/src/p'
            write(f'{root}/app/gen/classes/a/A.class', 'A')
            write(f'{root}/app/build/classes/kotlin/main/a/B.class', 'B')
            write(f'{root}/app/src/main/kotlin/A.kt', 'class A\nclass B\n')
            manifest = f'{work}/logs/p.kotlinc.compilations.tsv'
            write(manifest, ':app:compileCustomKotlin\tapp/gen/classes\n'
                            ':app:compileKotlin\tapp/build/classes/kotlin/main\n')
            outputs.snapshot(manifest, root, f'{work}/out/p/kotlinc')
            self.assertEqual(read(f'{work}/out/p/kotlinc/app/gen/classes/a/A.class'), 'A')
            self.assertEqual(read(f'{work}/out/p/kotlinc/app/build/classes/kotlin/main/a/B.class'), 'B')
            self.assertFalse(os.path.exists(f'{root}/app/gen/classes'))
            self.assertFalse(os.path.exists(f'{root}/app/build/classes/kotlin/main'))
            self.assertTrue(os.path.exists(f'{root}/app/src/main/kotlin/A.kt'))

            # The second build compiles only the standard destination: the kotlinc classes of the
            # custom one are gone, so they cannot be read as krusty's.
            write(f'{root}/app/build/classes/kotlin/main/a/B.class', 'B')
            krusty = f'{work}/logs/p.krusty.compilations.tsv'
            write(krusty, read(manifest))
            outputs.snapshot(krusty, root, f'{work}/out/p/krusty')
            self.assertFalse(os.path.exists(f'{work}/out/p/krusty/app/gen/classes'))
            subprocess.run([sys.executable, os.path.join(HERE, 'compare.py'), work, 'p', '0'],
                           check=True, stdout=subprocess.DEVNULL)
            with open(f'{work}/out/p/compare.json') as f:
                rows = {r['task']: r for r in json.load(f)['rows']}
            self.assertEqual((rows[':app:compileCustomKotlin']['built'],
                              rows[':app:compileCustomKotlin']['identical']), (False, 0))
            self.assertEqual((rows[':app:compileKotlin']['built'],
                              rows[':app:compileKotlin']['identical']), (True, 1))

    def test_an_output_that_is_not_task_owned_is_rejected_before_anything_moves(self):
        cases = {
            'outside the root': ':a:compileKotlin\t../elsewhere\n',
            'absolute': ':a:compileKotlin\t/tmp/classes\n',
            'the root itself': ':a:compileKotlin\t.\n',
            'shared': ':a:compileKotlin\tout\n:b:compileKotlin\tout\n',
            'nested': ':a:compileKotlin\tout\n:b:compileKotlin\tout/b\n',
            'malformed': ':a:compileKotlin\n',
        }
        for case, manifest_text in cases.items():
            with self.subTest(case), tempfile.TemporaryDirectory() as work:
                write(f'{work}/root/out/A.class', 'A')
                write(f'{work}/root/out/b/B.class', 'B')
                write(f'{work}/m.tsv', manifest_text)
                with self.assertRaises(outputs.InventoryError):
                    outputs.snapshot(f'{work}/m.tsv', f'{work}/root', f'{work}/snapshot')
                self.assertEqual(read(f'{work}/root/out/A.class'), 'A')
                self.assertFalse(os.path.exists(f'{work}/snapshot'))


class Pin(unittest.TestCase):
    def test_exactly_one_assignment_is_rewritten(self):
        with tempfile.TemporaryDirectory() as work:
            write(f'{work}/libs.versions.toml', '[versions]\nkotlin = "2.1.0"\nkotlinx = "1.0"\n')
            pin.rewrite(f'{work}/libs.versions.toml', 'kotlin', '2.4.21')
            self.assertEqual(read(f'{work}/libs.versions.toml'),
                             '[versions]\nkotlin = "2.4.21"\nkotlinx = "1.0"\n')
            write(f'{work}/gradle.properties', 'kotlin_version=2.1.0\nother=1\n')
            pin.rewrite(f'{work}/gradle.properties', 'kotlin_version', '2.4.21')
            self.assertEqual(read(f'{work}/gradle.properties'), 'kotlin_version=2.4.21\nother=1\n')

    def test_an_unmatched_or_repeated_key_fails_and_leaves_the_file(self):
        for text in ('[versions]\nkotlinVersion = "2.1.0"\n', 'kotlin = "1"\nkotlin = "2"\n'):
            with self.subTest(text), tempfile.TemporaryDirectory() as work:
                write(f'{work}/libs.versions.toml', text)
                with self.assertRaises(ValueError):
                    pin.rewrite(f'{work}/libs.versions.toml', 'kotlin', '2.4.21')
                self.assertEqual(read(f'{work}/libs.versions.toml'), text)


class Setup(unittest.TestCase):
    """`run.sh` against a local repository: a failed setup step ends the run before any build."""

    def run_corpus(self, work, commit, pin_spec):
        projects = f'{work}/projects.tsv'
        write(projects, f'p\tfile://{work}/upstream\t{commit}\t.\t21\t{pin_spec}\n')
        env = dict(os.environ, CORPUS_PROJECTS=projects)
        return subprocess.run(['bash', os.path.join(HERE, 'run.sh'), 'p', f'{work}/corpus',
                               '/nonexistent/krusty', '/nonexistent/repository', '0'],
                              env=env, capture_output=True, text=True)

    def upstream(self, work):
        repo = f'{work}/upstream'
        write(f'{repo}/gradle.properties', 'kotlin.code.style=official\n')
        write(f'{repo}/gradlew', '#!/bin/sh\necho gradle ran >&2\nexit 0\n')
        git = ['git', '-C', repo, '-c', 'user.name=t', '-c', 'user.email=t@t']
        subprocess.run(['git', 'init', '-q', repo], check=True)
        subprocess.run(git + ['add', '.'], check=True)
        subprocess.run(git + ['commit', '-qm', 'init'], check=True)
        return subprocess.run(git + ['rev-parse', 'HEAD'], check=True, capture_output=True,
                              text=True).stdout.strip()

    def assert_stopped_before_building(self, work, result, message):
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn(message, result.stderr)
        self.assertFalse(os.path.exists(f'{work}/corpus/logs/STATUS'))

    def test_a_failed_fetch_stops_before_any_build(self):
        with tempfile.TemporaryDirectory() as work:
            self.upstream(work)
            result = self.run_corpus(work, '0' * 40, '-')
            self.assert_stopped_before_building(work, result, '')

    def test_an_unmatched_version_pin_stops_before_any_build(self):
        with tempfile.TemporaryDirectory() as work:
            commit = self.upstream(work)
            result = self.run_corpus(work, commit, 'gradle.properties:kotlin_version')
            self.assert_stopped_before_building(
                work, result, "expected exactly one assignment of 'kotlin_version', found 0")
            self.assertEqual(read(f'{work}/corpus/src/p/gradle.properties'),
                             'kotlin.code.style=official\n')


if __name__ == '__main__':
    unittest.main()
