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


def compare(name, built, cost=None):
    return json.dumps(dict(name=name, reference_exit=0, built=built, compilations=2, identical=built,
                           classes=4, rows=[], **(dict(cost=cost) if cost else {})))


def build(compile_, peak, wall):
    return dict(built_compile_seconds=compile_, compile_seconds=compile_, wall_seconds=wall, cpu_seconds=wall,
                peak_rss_mb=dict(total=peak, gradle=0, kotlin_daemon=0, krusty=0, other=0))


class Publish(unittest.TestCase):
    def test_a_run_replaces_only_what_it_measured(self):
        with tempfile.TemporaryDirectory() as work:
            tree = f'{work}/tree'
            write(f'{tree}/projects/kept/compare.json', compare('kept', 1))
            write(f'{tree}/projects/gone/compare.json', compare('gone', 1))
            write(f'{tree}/projects/fresh/compare.json', compare('fresh', 0))
            # An earlier header without the cost columns.
            write(f'{tree}/history.tsv', 'date\tproject\n' + 'old line\n')
            write(f'{work}/run/corpus-fresh/out/fresh/compare.json', compare('fresh', 2, dict(
                kotlinc=build(30.5, 2500, 120.0), krusty=build(10.0, 1400, 90.0))))
            write(f'{work}/run/corpus-fresh/logs/fresh.plugins.tsv', 'gradle\t:\tp\n')
            write(f'{work}/run/corpus-fresh/logs/fresh.krusty.svg', '<svg/>')
            source = dict(url='https://example.test/fresh', commit='f00d', patch=None, kotlin_pin=None)
            names = publish.publish(f'{work}/run', tree, {'kept': {}, 'fresh': source}, 'abc123', '2026-10-25',
                                    'https://example.test/run/9')
            self.assertEqual(names, ['fresh'])
            self.assertEqual(sorted(os.listdir(f'{tree}/projects')), ['fresh', 'kept'])
            with open(f'{tree}/projects/fresh/compare.json') as f:
                fresh = json.load(f)
            self.assertEqual((fresh['built'], fresh['measured']),
                             (2, dict(date='2026-10-25', commit='abc123', run='https://example.test/run/9')))
            self.assertEqual(fresh['source'], source)
            self.assertEqual(fresh['flame_graph'], 'krusty-flamegraph.svg')
            with open(f'{tree}/projects/fresh/krusty-flamegraph.svg') as f:
                self.assertEqual(f.read(), '<svg/>')
            with open(f'{tree}/projects/fresh/fresh.plugins.tsv') as f:
                self.assertEqual(f.read(), 'gradle\t:\tp\n')
            with open(f'{tree}/projects/kept/compare.json') as f:
                self.assertNotIn('measured', json.load(f))
            with open(f'{tree}/history.tsv') as f:
                self.assertEqual(f.read(), publish.HISTORY_HEADER + 'old line\n'
                                 '2026-10-25\tabc123\tfresh\t0\t2\t2\t2\t4\thttps://example.test/run/9\t'
                                 '30.5\t10.0\t2500\t1400\t120.0\t90.0\n')


    def test_listed_records_what_run_sh_builds(self):
        with tempfile.TemporaryDirectory() as work:
            write(f'{work}/projects.tsv', '# comment\n'
                  'patched\thttps://example.test/p\tc0ffee\t.\t21\tgradle/libs.versions.toml:kotlin\n'
                  'plain\thttps://example.test/q\tbeef\tsub\t17\t-\tdemoDebug\n')
            write(f'{work}/scripts/oss-corpus/patches/patched.patch', '')
            projects = publish.listed(f'{work}/projects.tsv', work)
            self.assertEqual(projects, {
                'patched': dict(url='https://example.test/p', commit='c0ffee',
                                patch='scripts/oss-corpus/patches/patched.patch',
                                kotlin_pin='gradle/libs.versions.toml:kotlin'),
                'plain': dict(url='https://example.test/q', commit='beef', patch=None, kotlin_pin=None)})


if __name__ == '__main__':
    unittest.main()
