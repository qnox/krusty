#!/usr/bin/env python3
"""bench.py: the benchmarked compilations and edited file, the edits, a session against a stand-in
Gradle with a daemon that outlives each build, and the report."""
import json
import os
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import bench  # noqa: E402


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'w') as f:
        f.write(data)


def read(path):
    with open(path) as f:
        return f.read()


class Plan(unittest.TestCase):
    def test_only_compilations_krusty_built_are_benchmarked_and_the_edit_is_a_median_main_source(self):
        with tempfile.TemporaryDirectory() as work:
            root, logs = f'{work}/root', f'{work}/logs'
            write(f'{logs}/p.probe.compilations.tsv', ''.join(
                f':{m}:{t}\t{m}/build/classes/{t}\n' for m in 'abcde' for t in ('compileKotlin', 'compileTestKotlin')))
            write(f'{logs}/p.probe.tasks.tsv', ''.join([
                ':a:compileKotlinWithKrusty\t0\t1\texecuted\n',
                ':a:compileTestKotlinWithKrusty\t0\t1\texecuted\n',
                ':b:compileKotlinWithKrusty\t0\t1\tfailed\n',
                ':c:compileKotlinWithKrusty\t0\t1\tskipped\n',
                ':c:compileKotlin\t0\t1\texecuted\n',
                ':d:compileKotlinWithKrusty\t0\t1\texecuted\n',
            ]))
            write(f'{logs}/p.probe.log', "krusty: task ':d:compileKotlin' is left to kotlinc: compiler plugin x\n")
            sources = []
            for rel, size in (('a/src/A.kt', 10), ('a/src/B.kt', 30), ('a/src/C.kt', 20),
                              ('a/build/generated/G.kt', 25)):
                write(f'{root}/{rel}', 'x' * size)
                sources.append(f':a:compileKotlin\t{root}/{rel}\n')
            write(f'{work}/elsewhere/E.kt', 'x')
            sources.append(f':a:compileKotlin\t{work}/elsewhere/E.kt\n')
            for i in range(5):
                write(f'{root}/a/test/T{i}.kt', 'x')
                sources.append(f':a:compileTestKotlin\t{root}/a/test/T{i}.kt\n')
            write(f'{logs}/p.probe.sources.tsv', ''.join(sources))

            bench.plan(logs, 'p', root, f'{work}/plan.json')
            plan = json.loads(read(f'{work}/plan.json'))
            self.assertEqual(plan, dict(project='p', tasks=[':a:compileKotlin', ':a:compileTestKotlin'],
                                        edit='a/src/C.kt', edit_task=':a:compileKotlin', compilations=10))

            os.environ['BENCH_EDIT'] = 'a/src/A.kt'
            try:
                bench.plan(logs, 'p', root, f'{work}/plan.json')
            finally:
                del os.environ['BENCH_EDIT']
            self.assertEqual(json.loads(read(f'{work}/plan.json'))['edit'], 'a/src/A.kt')


class Edits(unittest.TestCase):
    def test_a_body_edit_keeps_its_declaration_and_an_abi_edit_renames_it(self):
        self.assertEqual(bench.edited('package p', 'body', 1), 'package p\n\nprivate fun corpusBenchBody(): Int = 1\n')
        self.assertEqual(bench.edited('package p\n', 'body', 2), 'package p\n\nprivate fun corpusBenchBody(): Int = 2\n')
        self.assertEqual(bench.edited('package p\n', 'abi', 3), 'package p\n\npublic fun corpusBenchAbi3(): Int = 3\n')


# A stand-in `gradlew`: the first build starts a detached daemon that holds 64 MB, every build logs
# its arguments and the edited file, `--stop` stops the daemon, and `$FAIL_ON` names an
# invocation (1-based) that fails.
GRADLEW = textwrap.dedent('''\
    #!/usr/bin/env bash
    if [ "$1" = --stop ]; then
      [ -f daemon.pid ] && kill "$(cat daemon.pid)" && rm daemon.pid
      exit 0
    fi
    n=$(( $(wc -l < calls.log 2>/dev/null || echo 0) + 1 ))
    echo "$* | $(tail -n 1 src/A.kt)" >> calls.log
    if [ ! -f daemon.pid ]; then
      setsid python3 -c "import time; b = bytearray(64 << 20); time.sleep(600)" </dev/null >/dev/null 2>&1 &
      echo $! > daemon.pid
      sleep 0.5
    fi
    printf ':a:compileKotlin\\t1000\\t3500\\texecuted\\n' > "$CORPUS_TASK_TIMES"
    [ "$n" = "${FAIL_ON:-0}" ] && exit 1
    exit 0
''')


class Session(unittest.TestCase):
    def run_session(self, work, **env):
        root = f'{work}/root'
        write(f'{root}/gradlew', GRADLEW)
        os.chmod(f'{root}/gradlew', 0o755)
        write(f'{root}/src/A.kt', 'package a\nclass A\n')
        write(f'{work}/plan.json', json.dumps(dict(tasks=[':a:compileKotlin'], edit='src/A.kt')))
        environment = dict(os.environ, BENCH_WARMUPS='1', BENCH_CLEAN_RUNS='1', BENCH_EDIT_RUNS='2', **env)
        code = subprocess.run([sys.executable, os.path.join(HERE, 'bench.py'), 'session', f'{work}/plan.json',
                               'kotlinc', f'{work}/kotlinc.json', '-Ix'],
                              cwd=root, env=environment, stderr=subprocess.DEVNULL).returncode
        return code, root

    def test_every_build_runs_against_one_daemon_which_is_measured_and_stopped(self):
        with tempfile.TemporaryDirectory() as work:
            code, root = self.run_session(work)
            result = json.loads(read(f'{work}/kotlinc.json'))
            calls = read(f'{root}/calls.log').splitlines()
            daemon_gone = not os.path.exists(f'{root}/daemon.pid')
            restored = read(f'{root}/src/A.kt')
        self.assertEqual(code, 0)
        self.assertIsNone(result['error'])
        self.assertEqual(calls, [
            '--daemon -Ix --no-build-cache --rerun-tasks :a:compileKotlin | class A',
            '--daemon -Ix --offline --no-build-cache --rerun-tasks :a:compileKotlin | class A',
            '--daemon -Ix --offline --no-build-cache :a:compileKotlin | private fun corpusBenchBody(): Int = 1',
            '--daemon -Ix --offline --no-build-cache :a:compileKotlin | private fun corpusBenchBody(): Int = 2',
            '--daemon -Ix --offline --no-build-cache :a:compileKotlin | private fun corpusBenchBody(): Int = 3',
            '--daemon -Ix --offline --no-build-cache :a:compileKotlin | public fun corpusBenchAbi4(): Int = 4',
            '--daemon -Ix --offline --no-build-cache :a:compileKotlin | public fun corpusBenchAbi5(): Int = 5',
            '--daemon -Ix --offline --no-build-cache :a:compileKotlin | public fun corpusBenchAbi6(): Int = 6',
        ])
        self.assertEqual({s: len(r) for s, r in result['runs'].items()}, dict(clean=1, body=2, abi=2))
        for runs in result['runs'].values():
            for r in runs:
                self.assertEqual((r['exit'], r['compile_seconds']), (0, 2.5))
                # The daemon an earlier build started is resident during and after this one.
                self.assertGreaterEqual(r['peak_rss_mb']['other'], 64)
                self.assertGreaterEqual(r['resident_after_mb']['total'], 64)
        self.assertTrue(daemon_gone, 'the session left its daemon running')
        self.assertEqual(restored, 'package a\nclass A\n')

    def test_a_failed_build_ends_the_session_and_restores_the_edited_file(self):
        with tempfile.TemporaryDirectory() as work:
            code, root = self.run_session(work, FAIL_ON='4')
            result = json.loads(read(f'{work}/kotlinc.json'))
            restored = read(f'{root}/src/A.kt')
            time.sleep(0.2)
        self.assertEqual(code, 1)
        self.assertEqual(result['error'], 'kotlinc body build 1 exited 1')
        self.assertEqual({s: len(r) for s, r in result['runs'].items()}, dict(clean=1, body=1, abi=0))
        self.assertEqual(restored, 'package a\nclass A\n')


class Report(unittest.TestCase):
    def test_the_report_takes_medians_and_peaks_of_successful_runs_and_no_figures_from_a_failed_session(self):
        def run(wall, rss):
            kinds = dict(total=rss, gradle=rss, kotlin_daemon=0, krusty=0, other=0)
            return dict(exit=0, wall_seconds=wall, compile_seconds=wall / 2, peak_rss_mb=kinds,
                        resident_after_mb=kinds)

        with tempfile.TemporaryDirectory() as work:
            write(f'{work}/p/plan.json', json.dumps(dict(tasks=[':a:compileKotlin'], compilations=3, edit='A.kt')))
            runs = dict(clean=[run(3, 100), run(1, 300), run(2, 200)], body=[run(1, 10)], abi=[])
            write(f'{work}/p/kotlinc.json', json.dumps(dict(mode='kotlinc', runs=runs, error=None)))
            failed = dict(clean=[run(1, 50)], body=[dict(run(0.5, 40), exit=1)], abi=[])
            write(f'{work}/p/krusty.json', json.dumps(dict(mode='krusty', runs=failed, error='krusty body build 1 exited 1')))
            subprocess.run([sys.executable, os.path.join(HERE, 'bench.py'), 'report', work, 'p'],
                           check=True, stdout=subprocess.DEVNULL)
            result = json.loads(read(f'{work}/p.json'))
        clean = result['modes']['kotlinc']['clean']
        self.assertEqual((clean['runs'], clean['wall_seconds'], clean['wall_seconds_range'], clean['compile_seconds'],
                          clean['peak_rss_mb']), (3, 2, [1, 3], 1, 300))
        self.assertIsNone(result['modes']['kotlinc']['abi'])
        self.assertEqual(result['modes']['krusty'], dict(error='krusty body build 1 exited 1',
                                                         clean=None, body=None, abi=None))
        self.assertIsNone(bench.stats([dict(run(1, 10), exit=1)]))


if __name__ == '__main__':
    unittest.main()
