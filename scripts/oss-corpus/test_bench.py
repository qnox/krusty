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


GRADLE_DAEMON = 'org.gradle.launcher.daemon.bootstrap.GradleDaemon'
KOTLIN_DAEMON = 'org.jetbrains.kotlin.daemon.KotlinCompileDaemon'

# A stand-in `gradlew` with Gradle's daemon contract: a build runs in the daemon registered in its
# Gradle user home when that one is alive, and otherwise starts one (64 MB) and registers it; the
# build's Kotlin compile likewise runs in the Kotlin daemon registered under `$XDG_DATA_HOME` (16
# MB). `$FOREIGN_REGISTRY` stands for a build handed to a daemon outside the session: the build
# then runs in the daemon registered there and starts none. Every build logs its arguments and the
# edited file, and `$FAIL_ON` names an invocation (1-based) that fails.
GRADLEW = textwrap.dedent('''\
    #!/usr/bin/env bash
    n=$(( $(wc -l < calls.log 2>/dev/null || echo 0) + 1 ))
    echo "$* | $(tail -n 1 src/A.kt)" >> calls.log
    echo "$GRADLE_RO_DEP_CACHE" > ro-cache.log
    daemon() {  # <registry> <main class> <MB>
      if [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null; then return; fi
      [ -n "$FOREIGN_REGISTRY" ] && return
      setsid python3 -c "import time; b = bytearray($3 << 20); time.sleep(600)" "$2" </dev/null >/dev/null 2>&1 &
      echo $! > "$1"
    }
    daemon "${FOREIGN_REGISTRY:-$GRADLE_USER_HOME/daemon.pid}" %s 64
    daemon "${FOREIGN_REGISTRY:-$XDG_DATA_HOME/kotlin.pid}" %s 16
    sleep 0.5
    printf ':a:compileKotlin\\t1000\\t3500\\texecuted\\n' > "$CORPUS_TASK_TIMES"
    [ "$n" = "${FAIL_ON:-0}" ] && exit 1
    exit 0
''' % (GRADLE_DAEMON, KOTLIN_DAEMON))


def alive(pid):
    try:
        with open(f'/proc/{pid}/stat') as f:
            return f.read().rsplit(')', 1)[1].split()[0] != 'Z'
    except OSError:
        return False


class Session(unittest.TestCase):
    def run_session(self, work, **env):
        root = f'{work}/root'
        write(f'{root}/gradlew', GRADLEW)
        os.chmod(f'{root}/gradlew', 0o755)
        write(f'{root}/src/A.kt', 'package a\nclass A\n')
        write(f'{work}/plan.json', json.dumps(dict(tasks=[':a:compileKotlin'], edit='src/A.kt')))
        environment = dict(os.environ, BENCH_WARMUPS='1', BENCH_CLEAN_RUNS='1', BENCH_EDIT_RUNS='2',
                           GRADLE_USER_HOME=f'{work}/shared', XDG_DATA_HOME=f'{work}/shared-xdg', **env)
        os.makedirs(f'{work}/shared', exist_ok=True)
        os.makedirs(f'{work}/shared-xdg', exist_ok=True)
        code = subprocess.run([sys.executable, os.path.join(HERE, 'bench.py'), 'session', f'{work}/plan.json',
                               'kotlinc', f'{work}/kotlinc.json', '-Ix'],
                              cwd=root, env=environment, stderr=subprocess.DEVNULL).returncode
        return code, root

    def start_outside(self, registry, main):
        """A compatible daemon started outside the session, registered where the caller's builds
        would find it."""
        daemon = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(600)', main],
                                  start_new_session=True)
        self.addCleanup(daemon.wait)
        self.addCleanup(daemon.kill)
        write(registry, str(daemon.pid))
        return daemon.pid

    def test_every_build_runs_in_the_sessions_own_daemons_which_are_measured_and_stopped(self):
        with tempfile.TemporaryDirectory() as work:
            outside = [self.start_outside(f'{work}/shared/daemon.pid', GRADLE_DAEMON),
                       self.start_outside(f'{work}/shared-xdg/kotlin.pid', KOTLIN_DAEMON)]
            code, root = self.run_session(work)
            result = json.loads(read(f'{work}/kotlinc.json'))
            calls = read(f'{root}/calls.log').splitlines()
            ro_cache = read(f'{root}/ro-cache.log').strip()
            restored = read(f'{root}/src/A.kt')
            home_left = os.path.exists(f'{work}/kotlinc.home')
            time.sleep(0.2)
            outside_alive = [alive(pid) for pid in outside]
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
        self.assertEqual(ro_cache, f'{work}/shared/caches')
        self.assertEqual({s: len(r) for s, r in result['runs'].items()}, dict(clean=1, body=2, abi=2))
        runs = [r for scenario in result['runs'].values() for r in scenario]
        owned = runs[0]['daemons']
        self.assertEqual(set(owned), {'gradle', 'kotlin'})
        self.assertFalse(set(owned.values()) & set(outside))
        for r in runs:
            self.assertEqual((r['exit'], r['compile_seconds'], r['daemons']), (0, 2.5, owned))
            # The daemons an earlier build started are resident during and after this one.
            self.assertGreaterEqual(r['peak_rss_mb']['gradle'], 64)
            self.assertGreaterEqual(r['peak_rss_mb']['kotlin_daemon'], 16)
            self.assertGreaterEqual(r['resident_after_mb']['total'], 80)
        self.assertFalse(any(alive(pid) for pid in owned.values()), 'the session left its daemons running')
        self.assertEqual(outside_alive, [True, True], 'the session stopped a daemon it does not own')
        self.assertFalse(home_left)
        self.assertEqual(restored, 'package a\nclass A\n')

    def test_a_build_served_by_a_daemon_outside_the_session_ends_it(self):
        with tempfile.TemporaryDirectory() as work:
            outside = self.start_outside(f'{work}/foreign.pid', GRADLE_DAEMON)
            code, root = self.run_session(work, FOREIGN_REGISTRY=f'{work}/foreign.pid')
            result = json.loads(read(f'{work}/kotlinc.json'))
            outside_alive = alive(outside)
        self.assertEqual(code, 1)
        self.assertEqual(result['error'], 'kotlinc: a build ran in 0 gradle daemons of this session, not one')
        self.assertTrue(outside_alive)

    def test_a_failed_build_ends_the_session_and_restores_the_edited_file(self):
        with tempfile.TemporaryDirectory() as work:
            code, root = self.run_session(work, FAIL_ON='4')
            result = json.loads(read(f'{work}/kotlinc.json'))
            restored = read(f'{root}/src/A.kt')
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
