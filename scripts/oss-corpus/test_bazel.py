#!/usr/bin/env python3
"""bazel.sh and bazel_compare.py: a Bazel project's two builds, its compile-action inventory, and
its comparison in compare.py's shape."""
import json
import os
import shlex
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import bazel_compare  # noqa: E402
import summarize  # noqa: E402

BIN = 'bazel-out/k8-fastbuild/bin'
SUMMARY_ROW = '| p |  | 2 | 0 | ok | 2 / 5 | 3 / 10 (30.0%) |'


def jar(path, entries):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, 'w') as z:
        for name, data in entries.items():
            z.writestr(name, data)


def spawn(label, mnemonic, inputs=(), walltime='2.500s', exit_code=0):
    package, name = label[2:].split(':')
    entry = dict(targetLabel=label, mnemonic=mnemonic, walltime=walltime,
                 listedOutputs=[f'{BIN}/{package}/{name}.jar', f'{BIN}/{package}/{name}.abi.jar'],
                 inputs=[dict(path=p) for p in inputs])
    if exit_code:
        entry['exitCode'] = exit_code
    return entry


def write_log(path, entries):
    # The JSON execution log is a stream of concatenated objects, not an array.
    Path(path).write_text(''.join(json.dumps(e, indent=1) + '\n' for e in entries))


class Snapshot(unittest.TestCase):
    def test_lists_the_compile_actions_and_copies_the_jars_that_exist(self):
        with tempfile.TemporaryDirectory() as work:
            checkout = Path(work, 'src')
            jar(checkout / BIN / 'a/a.jar', {'A.class': b'a'})
            jar(checkout / BIN / 'b/b.jar', {'B.class': b'b'})
            write_log(f'{work}/exec.json', [
                spawn('//a:a', 'KrustyCompile', walltime='1.250s'),
                spawn('//b:b', 'JvmCompile', inputs=['b/B.java', 'lib.jar']),
                spawn('//c:c', 'KrustyCompile', exit_code=1),
                dict(targetLabel='//a:a', mnemonic='Javac', listedOutputs=['x.jar']),
            ])
            bazel_compare.snapshot(f'{work}/exec.json', str(checkout), f'{work}/m.tsv', f'{work}/out')
            self.assertEqual(Path(work, 'm.tsv').read_text().splitlines(), [
                f'//a:a\t{BIN}/a/a.jar\tKrustyCompile\t1.25\t0\t',
                f'//b:b\t{BIN}/b/b.jar\tJvmCompile\t2.5\t0\tJava sources',
                f'//c:c\t{BIN}/c/c.jar\tKrustyCompile\t2.5\t1\t',
            ])
            copied = sorted(str(p.relative_to(f'{work}/out')) for p in Path(work, 'out').rglob('*') if p.is_file())
            self.assertEqual(copied, ['a/a.jar', 'b/b.jar'])

    def test_a_held_back_action_names_the_integration_condition_it_meets(self):
        self.assertEqual(bazel_compare.held_back(['x/A.kt', 'x/B.java']), 'Java sources')
        self.assertEqual(bazel_compare.held_back(['x/A.kt', 'gen.srcjar']), 'source jars')
        self.assertEqual(bazel_compare.held_back(['x/A.kt', 'compose-plugin.jar']), 'compiler plugins')


def manifest(path, rows):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    Path(path).write_text(''.join('\t'.join(map(str, row)) + '\n' for row in rows))


class Compare(unittest.TestCase):
    def test_writes_compare_json_in_the_gradle_shape(self):
        with tempfile.TemporaryDirectory() as work:
            name = 'p'
            out, logs, checkout = Path(work, 'out', name), Path(work, 'logs'), Path(work, 'src', name)
            checkout.mkdir(parents=True)
            Path(checkout, 'A.kt').write_text('fun a() = 1\n\nfun b() = 2\n')
            labels = ['//ok:ok', '//diff:diff', '//java:java', '//fail:fail', '//skip:skip', '//empty:empty']
            manifest(logs / f'{name}.kotlinc.compilations.tsv',
                     [(label, f'{BIN}/{label[2:].split(":")[0]}/{label.split(":")[1]}.jar', 'JvmCompile', 3.0, 0, '')
                      for label in labels])
            manifest(logs / f'{name}.krusty.compilations.tsv', [
                ('//ok:ok', f'{BIN}/ok/ok.jar', 'KrustyCompile', 1.0, 0, ''),
                ('//diff:diff', f'{BIN}/diff/diff.jar', 'KrustyCompile', 1.5, 0, ''),
                ('//java:java', f'{BIN}/java/java.jar', 'JvmCompile', 3.0, 0, 'Java sources'),
                ('//fail:fail', f'{BIN}/fail/fail.jar', 'KrustyCompile', 0.5, 1, ''),
                ('//empty:empty', f'{BIN}/empty/empty.jar', 'KrustyCompile', 0.1, 0, ''),
            ])
            for label in labels:
                package, target = label[2:].split(':')
                if label != '//empty:empty':
                    jar(out / 'kotlinc' / package / f'{target}.jar',
                        {f'{target}/A.class': b'same', f'{target}/B.class': b'kotlinc', 'META-INF/MANIFEST.MF': b'x'})
                else:
                    jar(out / 'kotlinc' / package / f'{target}.jar', {'META-INF/MANIFEST.MF': b'x'})
            jar(out / 'krusty/ok/ok.jar', {'ok/A.class': b'same', 'ok/B.class': b'kotlinc', 'META-INF/MANIFEST.MF': b'y'})
            jar(out / 'krusty/diff/diff.jar', {'diff/A.class': b'same', 'diff/B.class': b'krusty', 'diff/C.class': b'c'})
            jar(out / 'krusty/java/java.jar', {'java/A.class': b'same', 'java/B.class': b'kotlinc'})
            jar(out / 'krusty/empty/empty.jar', {'META-INF/MANIFEST.MF': b'x'})
            (logs / f'{name}.krusty.log').write_text(
                'INFO: Analyzed 6 targets\n'
                f'ERROR: {checkout}/fail/BUILD.bazel:1:12: compile //fail:fail with krusty (kt: 1) failed: '
                '(Exit 1): krusty.bin failed: error executing KrustyCompile command\n'
                'krusty: no effect on output: --opt_in kotlin.RequiresOptIn\n'
                f'{checkout}/fail/F.kt:3:5: error: unresolved reference \'x\'\n'
                'ERROR: Build did NOT complete successfully\n')
            for mode, total in (('kotlinc', 2048), ('krusty', 1024)):
                (logs / f'{name}.{mode}.metrics.json').write_text(json.dumps(dict(
                    exit=0 if mode == 'kotlinc' else 1, wall_seconds=100.0, cpu_seconds=300.0,
                    peak_rss_mb=dict(total=total, gradle=0, kotlin_daemon=900, krusty=300, other=100))))
            result = subprocess.run([sys.executable, str(HERE / 'bazel_compare.py'), 'compare', work, name, '0', '1'],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            data = json.loads((out / 'compare.json').read_text())

        def row(label, built, fallback, error, identical, only_kotlinc, only_krusty, krusty_seconds):
            package, target = label[2:].split(':')
            return dict(task=label, dir=f'{package}/{target}.jar', built=built, fallback=fallback, error=error,
                        classes=2, identical=identical, only_kotlinc=only_kotlinc, only_krusty=only_krusty,
                        seconds=dict(kotlinc=3.0, krusty=krusty_seconds), verify=None)
        self.assertEqual(data['rows'], [
            row('//diff:diff', True, None, None, 1, 0, 1, 1.5),
            row('//fail:fail', False, None, "fail/F.kt:3:5: error: unresolved reference 'x'", 0, None, None, 0.5),
            row('//java:java', False, 'Java sources', None, 0, None, None, 3.0),
            row('//ok:ok', True, None, None, 2, 0, 0, 1.0),
            row('//skip:skip', False, None, 'not run: a target it depends on failed', 0, None, None, None),
        ])
        self.assertEqual(data['no_reference_classes'], [dict(task='//empty:empty', dir='empty/empty.jar', krusty_classes=0)])
        self.assertEqual({k: data[k] for k in ('name', 'reference_exit', 'verify_exit', 'compilations', 'built',
                                               'classes', 'identical', 'hotspots', 'tests')},
                         dict(name='p', reference_exit=0, verify_exit=None, compilations=5, built=2, classes=10,
                              identical=3, hotspots=None, tests=None))
        self.assertEqual(data['loc'], dict(kotlin=2, java=0))
        self.assertEqual(data['cost']['krusty']['compile_seconds'], 6.0)
        self.assertEqual(data['cost']['krusty']['built_compile_seconds'], 2.5)
        self.assertEqual(data['cost']['kotlinc']['built_compile_seconds'], 6.0)
        # summarize.py reads it like a Gradle project's.
        report = summarize.report([data], {}, {}, None)
        self.assertIn(SUMMARY_ROW, report.splitlines())

    def test_a_krusty_build_that_ran_no_compile_says_why_once(self):
        with tempfile.TemporaryDirectory() as work:
            name = 'p'
            logs, out = Path(work, 'logs'), Path(work, 'out', name)
            Path(work, 'src', name).mkdir(parents=True)
            manifest(logs / f'{name}.kotlinc.compilations.tsv', [('//a:a', f'{BIN}/a/a.jar', 'JvmCompile', 1.0, 0, '')])
            jar(out / 'kotlinc/a/a.jar', {'A.class': b'a'})
            (logs / f'{name}.krusty.log').write_text(
                'ERROR: Error computing the main repository mapping: KRUSTY_BINARY does not exist: /x\n')
            rows, _ = bazel_compare.compare(work, name)
        self.assertEqual([r['error'] for r in rows],
                         ['not run: the krusty build failed: Error computing the main repository mapping: '
                          'KRUSTY_BINARY does not exist: /x'])


# A stand-in for the project's `bazel.cmd`: it records each invocation and, for a build, writes the
# execution log and the jars the krusty integration would leave (a KrustyCompile with the binary
# passed through `--repo_env`, the project's JvmCompile without it).
FAKE_BAZEL = r'''#!/usr/bin/env python3
import json, os, sys, zipfile
args = sys.argv[1:]
with open(os.environ['FAKE_BAZEL_CALLS'], 'a') as f:
    f.write(' '.join(a for a in args if not a.startswith('--output_user_root')) + '\n')
startup = [a for a in args if a.startswith('--output_user_root') or a.startswith('--host_jvm_args')]
command = args[len(startup)]
if command != 'build' or '--nobuild' in args:
    sys.exit(0)
log = next(a.split('=', 1)[1] for a in args if a.startswith('--execution_log_json_file='))
krusty = any(a.startswith('--repo_env=KRUSTY_BINARY=') for a in args)
targets = args[args.index('--') + 1:]
with open(log, 'w') as f:
    for label in targets:
        package, name = label[2:].split(':')
        out = f'bazel-out/k8-fastbuild/bin/{package}/{name}.jar'
        os.makedirs(os.path.dirname(out), exist_ok=True)
        with zipfile.ZipFile(out, 'w') as z:
            z.writestr(f'{name}/A.class', b'same')
        json.dump(dict(targetLabel=label, mnemonic='KrustyCompile' if krusty else 'JvmCompile',
                       walltime='1s', listedOutputs=[out]), f)
'''


class Runner(unittest.TestCase):
    def test_run_sh_builds_a_bazel_project_with_and_without_krusty(self):
        with tempfile.TemporaryDirectory() as work:
            repo = Path(work, 'upstream')
            repo.mkdir()
            Path(repo, 'bazel.cmd').write_text(
                f'exec {shlex.quote(sys.executable)} "$(dirname "$0")/fake_bazel.py" "$@"\n')
            Path(repo, 'fake_bazel.py').write_text(FAKE_BAZEL)
            Path(repo, 'MODULE.bazel').write_text('module(name = "p")\n')
            git = ['git', '-C', str(repo), '-c', 'user.name=t', '-c', 'user.email=t@t']
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            subprocess.run(git + ['add', '.'], check=True)
            subprocess.run(git + ['commit', '-qm', 'init'], check=True)
            commit = subprocess.run(git + ['rev-parse', 'HEAD'], check=True, capture_output=True,
                                    text=True).stdout.strip()
            plugin = Path(work, 'plugin')
            plugin.mkdir()
            Path(plugin, 'plugin.iml').write_text('<module/>\n')
            pgit = ['git', '-C', str(plugin), '-c', 'user.name=t', '-c', 'user.email=t@t']
            subprocess.run(['git', 'init', '-q', str(plugin)], check=True)
            subprocess.run(pgit + ['add', '.'], check=True)
            subprocess.run(pgit + ['commit', '-qm', 'init'], check=True)
            plugin_commit = subprocess.run(pgit + ['rev-parse', 'HEAD'], check=True, capture_output=True,
                                           text=True).stdout.strip()
            projects = Path(work, 'projects.tsv')
            projects.write_text(f'p\tfile://{repo}\t{commit}\t.\t21\t-\t-\tcompile,lsp\n')
            integration = Path(work, 'integration')
            integration.mkdir()
            Path(integration, '0001-marker.patch').write_text(
                'diff --git a/KRUSTY b/KRUSTY\nnew file mode 100644\n--- /dev/null\n+++ b/KRUSTY\n@@ -0,0 +1 @@\n+patched\n')
            bazel = Path(work, 'bazel', 'p')
            bazel.mkdir(parents=True)
            Path(bazel, 'targets').write_text('# comment\n//a:a\n\n//b:b  # trailing comment\n')
            Path(bazel, 'integration').write_text(f'{integration}\n')
            Path(bazel, 'checkouts').write_text(f'# a repository the build reads\nandroid\tfile://{plugin}\t{plugin_commit}\n')
            krusty = Path(work, 'krusty')
            krusty.write_text('#!/bin/sh\n')
            calls = Path(work, 'calls')
            env = dict(os.environ, CORPUS_PROJECTS=str(projects), CORPUS_BAZEL=str(Path(work, 'bazel')),
                       FAKE_BAZEL_CALLS=str(calls))
            result = subprocess.run(['bash', str(HERE / 'run.sh'), 'p', f'{work}/corpus', str(krusty),
                                     '/nonexistent/repository', '0'], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(Path(work, 'corpus/src/p/KRUSTY').read_text(), 'patched\n')
            self.assertEqual(Path(work, 'corpus/src/p/android/plugin.iml').read_text(), '<module/>\n')
            common = '--disk_cache= --color=no --curses=no --show_progress_rate_limit=60'
            measured = (f'--keep_going --repository_disable_download --execution_log_json_file={work}/corpus/logs/'
                        '{mode}.exec.json --execution_log_sort=false -- //a:a //b:b')
            self.assertEqual(calls.read_text().splitlines(), [
                f'--host_jvm_args=-Xmx6g build {common} --nobuild -- //a:a //b:b',
                '--host_jvm_args=-Xmx6g clean',
                '--host_jvm_args=-Xmx6g shutdown',
                f'--host_jvm_args=-Xmx6g build {common} ' + measured.format(mode='p.kotlinc'),
                '--host_jvm_args=-Xmx6g clean',
                '--host_jvm_args=-Xmx6g shutdown',
                f'--host_jvm_args=-Xmx6g build {common} --repo_env=KRUSTY_BINARY={krusty} '
                + measured.format(mode='p.krusty'),
                '--host_jvm_args=-Xmx6g shutdown',
            ])
            data = json.loads(Path(work, 'corpus/out/p/compare.json').read_text())
            self.assertEqual((data['compilations'], data['built'], data['classes'], data['identical']), (2, 2, 2, 2))
            self.assertEqual(sorted(p.name for p in Path(work, 'corpus/logs').iterdir()), [
                'STATUS', 'p.kotlinc.compilations.tsv', 'p.kotlinc.log', 'p.kotlinc.metrics.json',
                'p.krusty.compilations.tsv', 'p.krusty.log', 'p.krusty.metrics.json', 'p.warmup.log'])

    def test_a_target_set_without_targets_stops_before_checkout(self):
        with tempfile.TemporaryDirectory() as work:
            bazel = Path(work, 'bazel', 'p')
            bazel.mkdir(parents=True)
            Path(bazel, 'targets').write_text('# nothing\n')
            Path(bazel, 'integration').write_text('tools/krusty-bazel/intellij\n')
            result = subprocess.run(['bash', str(HERE / 'bazel.sh'), 'p', f'{work}/corpus', '/x/krusty'],
                                    env=dict(os.environ, CORPUS_BAZEL=str(Path(work, 'bazel'))),
                                    capture_output=True, text=True)
            self.assertEqual((result.returncode, result.stderr), (2, 'p: bazel/p/targets lists no target\n'))
            self.assertFalse(Path(work, 'corpus/src').exists())

    def test_the_checked_in_target_set_points_at_the_intellij_integration(self):
        bazel = HERE / 'bazel' / 'intellij-community'
        targets = [line.split('#')[0].strip() for line in (bazel / 'targets').read_text().splitlines()]
        targets = [t for t in targets if t]
        self.assertTrue(targets)
        self.assertEqual(len(targets), len(set(targets)))
        for target in targets:
            self.assertRegex(target, r'^//[^:\s]*:[^:\s]+$')
        integration = HERE.parents[1] / (bazel / 'integration').read_text().strip()
        self.assertTrue(list(integration.glob('*.patch')), integration)


if __name__ == '__main__':
    unittest.main()
