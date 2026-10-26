#!/usr/bin/env python3
"""publish.py: a run replaces the entries of the projects it measured, keeps the others, drops
projects no longer listed, and appends one history line per measured project."""
import json
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import publish  # noqa: E402


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'w') as f:
        f.write(data)


def compare(name, built):
    return json.dumps(dict(name=name, reference_exit=0, built=built, compilations=2, identical=built,
                           classes=4, rows=[]))


class Publish(unittest.TestCase):
    def test_a_run_replaces_only_what_it_measured(self):
        with tempfile.TemporaryDirectory() as work:
            tree = f'{work}/tree'
            write(f'{tree}/projects/kept/compare.json', compare('kept', 1))
            write(f'{tree}/projects/gone/compare.json', compare('gone', 1))
            write(f'{tree}/projects/fresh/compare.json', compare('fresh', 0))
            write(f'{tree}/history.tsv', publish.HISTORY_HEADER + 'old line\n')
            write(f'{work}/run/corpus-fresh/out/fresh/compare.json', compare('fresh', 2))
            write(f'{work}/run/corpus-fresh/logs/fresh.plugins.tsv', 'gradle\t:\tp\n')
            names = publish.publish(f'{work}/run', tree, {'kept', 'fresh'}, 'abc123', '2026-10-25',
                                    'https://example.test/run/9')
            self.assertEqual(names, ['fresh'])
            self.assertEqual(sorted(os.listdir(f'{tree}/projects')), ['fresh', 'kept'])
            with open(f'{tree}/projects/fresh/compare.json') as f:
                fresh = json.load(f)
            self.assertEqual((fresh['built'], fresh['measured']),
                             (2, dict(date='2026-10-25', commit='abc123', run='https://example.test/run/9')))
            with open(f'{tree}/projects/fresh/fresh.plugins.tsv') as f:
                self.assertEqual(f.read(), 'gradle\t:\tp\n')
            with open(f'{tree}/projects/kept/compare.json') as f:
                self.assertNotIn('measured', json.load(f))
            with open(f'{tree}/history.tsv') as f:
                self.assertEqual(f.read(), publish.HISTORY_HEADER + 'old line\n'
                                 '2026-10-25\tabc123\tfresh\t0\t2\t2\t2\t4\thttps://example.test/run/9\n')


if __name__ == '__main__':
    unittest.main()
