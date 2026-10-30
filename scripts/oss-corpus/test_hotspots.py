#!/usr/bin/env python3
"""hotspots.py: `perf script` stacks fold root first without Rust hashes, and rank by self time."""
import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import hotspots  # noqa: E402

PERF_SCRIPT = '''\
krusty
\t    55d0a1 memchr::memmem::Finder::find::h0123456789abcdef+0x21 (/w/krusty)
\t    55d0b2 zip::read::ZipArchive::new+0x10 (/w/krusty)
\t    55d0c3 krusty::main+0x5 (/w/krusty)

krusty
\t    55d0a1 memchr::memmem::Finder::find::h0123456789abcdef+0x30 (/w/krusty)
\t    55d0b2 zip::read::ZipArchive::new+0x10 (/w/krusty)
\t    55d0c3 krusty::main+0x5 (/w/krusty)

krusty
\t    7f0001 [unknown] (/usr/lib/x86_64-linux-gnu/libc.so.6)
\t    55d0c3 krusty::main+0x9 (/w/krusty)
'''


class Hotspots(unittest.TestCase):
    def test_stacks_fold_root_first(self):
        self.assertEqual(hotspots.fold(PERF_SCRIPT.splitlines(keepends=True)), {
            'krusty::main;zip::read::ZipArchive::new;memchr::memmem::Finder::find': 2,
            'krusty::main;[libc.so.6]': 1,
        })

    def test_functions_rank_by_their_own_samples(self):
        stacks = hotspots.fold(PERF_SCRIPT.splitlines(keepends=True))
        self.assertEqual(hotspots.rank(stacks, limit=2), dict(samples=3, functions=[
            dict(function='memchr::memmem::Finder::find', self=2, total=2),
            dict(function='[libc.so.6]', self=1, total=1),
        ]))


if __name__ == '__main__':
    unittest.main()
