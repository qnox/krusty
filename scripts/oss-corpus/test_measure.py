#!/usr/bin/env python3
"""measure.py: records the command's exit, time and memory, and stops the daemons it leaves behind."""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


class Measure(unittest.TestCase):
    def test_a_daemon_the_command_leaves_behind_is_measured_and_stopped(self):
        with tempfile.TemporaryDirectory() as work:
            pid_file = f'{work}/daemon.pid'
            # The daemon detaches from its parent, which exits without waiting for it.
            command = ['sh', '-c', f'(python3 -c "import time; b = bytearray(64 << 20); time.sleep(600)" & '
                                   f'echo $! > {pid_file}); sleep 1; exit 3']
            code = subprocess.run([sys.executable, os.path.join(HERE, 'measure.py'), f'{work}/m.json', *command]).returncode
            with open(f'{work}/m.json') as f:
                metrics = json.load(f)
            with open(pid_file) as f:
                daemon = int(f.read())
        self.assertEqual((code, metrics['exit']), (3, 3))
        self.assertGreaterEqual(metrics['wall_seconds'], 1)
        self.assertGreaterEqual(metrics['peak_rss_mb']['other'], 64)
        self.assertGreaterEqual(metrics['peak_rss_mb']['total'], metrics['peak_rss_mb']['other'])
        time.sleep(0.2)
        self.assertFalse(os.path.exists(f'/proc/{daemon}'), 'the daemon outlived the measurement')

    def test_the_bazel_builders_compile_worker_counts_as_kotlinc(self):
        sys.path.insert(0, HERE)
        import measure
        # intellij-community's JvmCompile worker: the JDK running the launcher with the builder's jar.
        worker = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)', '--',
                                   'external/rules_jvm+/rules/impl/MemoryLauncher.java',
                                   'bazel-out/k8-opt-exec/bin/external/rules_jvm+/jvm-inc-builder/'
                                   'jvm-inc-builder_deploy.jar'])
        try:
            time.sleep(0.2)
            self.assertEqual(measure.kind(worker.pid), 'kotlin_daemon')
        finally:
            worker.kill()
            worker.wait()


if __name__ == '__main__':
    unittest.main()
