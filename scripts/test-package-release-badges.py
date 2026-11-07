#!/usr/bin/env python3
"""Execute release badge packaging against complete and incomplete report inputs."""
import json
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("package-release-badges.sh")


class ReleaseBadgeAssets(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.reports = self.root / "reports with spaces"
        self.output = self.root / "dist with spaces"
        self.reports.mkdir()
        self.output.mkdir()
        self.inputs = {
            "pct.txt": "75.0 3 4\n",
            "jvm-byte-equality.txt": "25.0 1 4\n",
            "native-pct.txt": "50.0 2 4\n",
        }
        self.reset_reports()

    def reset_reports(self):
        for name, text in self.inputs.items():
            (self.reports / name).write_text(text)

    def package(self, version="2.4.21-RC1"):
        return subprocess.run(
            ["bash", str(SCRIPT), version, str(self.reports), str(self.output)],
            capture_output=True,
            text=True,
        )

    def test_all_four_exact_assets_keep_the_reports_and_version(self):
        result = self.package()
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, "", ""))
        expected = {
            "krusty-conformance.json": {
                "schemaVersion": 1, "label": "Kotlin 2.4.21-RC1 conformance",
                "message": "75.0% (3/4)", "color": "brightgreen",
            },
            "krusty-jvm-byte-equality.json": {
                "schemaVersion": 1, "label": "Kotlin 2.4.21-RC1 JVM byte equality",
                "message": "25.0% (1/4 bytes)", "color": "orange",
            },
            "krusty-native-conformance.json": {
                "schemaVersion": 1, "label": "Kotlin 2.4.21-RC1 Native conformance",
                "message": "50.0% (2/4)", "color": "yellow",
            },
            "krusty-kotlin.json": {
                "schemaVersion": 1, "label": "Kotlin",
                "message": "2.4.21-RC1", "color": "blue",
            },
        }
        self.assertEqual(sorted(p.name for p in self.output.iterdir()), sorted(expected))
        for name, value in expected.items():
            self.assertEqual(
                (self.output / name).read_text(),
                json.dumps(value, separators=(",", ":")) + "\n",
            )

    def test_each_missing_report_preserves_the_existing_output(self):
        (self.output / "existing").write_text("keep")
        for name in self.inputs:
            with self.subTest(report=name):
                self.reset_reports()
                (self.reports / name).unlink()
                result = self.package()
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, "")
                self.assertEqual(
                    {p.name: p.read_text() for p in self.output.iterdir()},
                    {"existing": "keep"},
                )

    def test_a_late_inconsistent_report_publishes_no_partial_package(self):
        (self.reports / "native-pct.txt").write_text("99.0 2 4\n")
        result = self.package()
        self.assertEqual((result.returncode, result.stdout), (1, ""))
        self.assertEqual(list(self.output.iterdir()), [])

    def test_an_invalid_version_publishes_no_package(self):
        result = self.package("2.4.21\ninvalid")
        self.assertEqual((result.returncode, result.stdout), (2, ""))
        self.assertEqual(list(self.output.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
