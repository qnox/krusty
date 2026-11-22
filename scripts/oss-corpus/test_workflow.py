#!/usr/bin/env python3
"""Execute the corpus summary step and check its generated reports."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

from test_summarize import project, row

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


class Workflow(unittest.TestCase):
    def test_actual_summary_step_retains_both_formats_for_this_run(self):
        workflow = (ROOT / ".github/workflows/oss-corpus.yml").read_text()
        step = workflow.split("      - name: summarize\n", 1)[1].split("\n      - ", 1)[0]
        script = "\n".join(line[10:] for line in step.split("run: |\n", 1)[1].splitlines()
                           if line.startswith("          "))
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            target = root / "scripts/oss-corpus/summarize.py"
            target.parent.mkdir(parents=True)
            shutil.copyfile(HERE / "summarize.py", target)
            inputs = [project("zeta", [row(":compileKotlin", True, 4, 4)]),
                      project("Alpha", [row(":compileKotlin", True, 2, 2)])]
            for value in inputs:
                path = root / "results" / value["name"] / "compare.json"
                path.parent.mkdir(parents=True)
                path.write_text(json.dumps(value))
            # The stars lookup lists the whole compile lane; only the projects this run was asked to
            # measure are expected, and ghost's job left nothing behind.
            (root / "stars.tsv").write_text("zeta\t\nAlpha\t\nghost\t\nunselected\t\n")
            output = subprocess.run(["bash", "-euo", "pipefail", "-c", script], cwd=root,
                                    env={**os.environ, "GITHUB_STEP_SUMMARY": str(root / "job-summary.md"),
                                         "EXPECTED": '["Alpha","zeta","ghost"]'},
                                    capture_output=True, timeout=10)
            self.assertEqual(output.returncode, 0, output)
            self.assertEqual((root / "summary.md").read_bytes(), (root / "job-summary.md").read_bytes())
            document = json.loads((root / "summary.json").read_text())
            self.assertEqual([value["name"] for value in document["projects"]], ["Alpha", "zeta"])
            for actual, original in zip(document["projects"], reversed(inputs)):
                self.assertEqual({key: actual[key] for key in original}, original)
            self.assertEqual(document["not_measured"], [
                {"name": "ghost",
                 "reason": "its job uploaded no logs: it stopped before a build ran, or the artifact is missing"}])
            self.assertIn("| ghost | its job uploaded no logs", (root / "summary.md").read_text())

    def check_step(self, document):
        workflow = (ROOT / ".github/workflows/oss-corpus.yml").read_text()
        step = workflow.split("      - name: fail when a project was not measured\n", 1)[1].split("\n      - ", 1)[0]
        script = "\n".join(line[10:] for line in step.split("run: |\n", 1)[1].splitlines()
                           if line.startswith("          "))
        with tempfile.TemporaryDirectory() as work:
            (Path(work) / "summary.json").write_text(json.dumps(document))
            return subprocess.run(["bash", "-euo", "pipefail", "-c", script], cwd=work,
                                  capture_output=True, text=True, timeout=10)

    def test_a_run_with_an_unmeasured_project_fails_after_publishing_its_summary(self):
        workflow = (ROOT / ".github/workflows/oss-corpus.yml").read_text()
        self.assertLess(workflow.index("name: summary\n"), workflow.index("fail when a project was not measured"))
        done = self.check_step(dict(projects=[], not_measured=[]))
        self.assertEqual((done.returncode, done.stdout), (0, ""))
        failed = self.check_step(dict(projects=[], not_measured=[
            dict(name="okio", reason="the warmup build exited 1 after 22 s"),
            dict(name="koin", reason="its job uploaded no logs")]))
        self.assertEqual(failed.returncode, 1)
        self.assertEqual(failed.stdout.splitlines(), [
            "::error title=okio was not measured::the warmup build exited 1 after 22 s",
            "::error title=koin was not measured::its job uploaded no logs"])


if __name__ == "__main__":
    unittest.main()
