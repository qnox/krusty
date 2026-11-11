#!/usr/bin/env python3
"""The parity summary keeps unmeasured latencies unset and does not treat an old record as a failed open."""

import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "parity-scan.py"


def load():
    spec = importlib.util.spec_from_file_location("parity_scan", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SCAN = load()


def report(**extra):
    base = {
        "module": "app",
        "status": "ok",
        "checked_files": 1,
        "elapsed_ms": 4,
        "error_count": 0,
        "diagnostics": [],
    }
    base.update(extra)
    return base


class ParityMetrics(unittest.TestCase):
    def test_latencies_stay_unset_and_a_missing_open_field_is_not_zero(self):
        summary = SCAN.summarize([report()], top=5)
        self.assertIsNone(summary["parse_ms"])
        self.assertIsNone(summary["type_info_ms"])
        self.assertIsNone(summary["navigation_ms"])
        self.assertIsNone(summary["open_complete_modules"])
        self.assertIsNone(summary["missing_classpath_entries"])
        text = SCAN.render_text(summary)
        self.assertIn("open         not measured", text)
        self.assertIn("parse        not measured", text)
        self.assertIn("navigation   not measured", text)

    def test_open_completeness_counts_only_records_that_say_so(self):
        summary = SCAN.summarize(
            [
                report(open_complete=True, missing_classpath_entries=0),
                report(module="lib", status="errors", error_count=2, open_complete=False, missing_classpath_entries=3),
            ],
            top=5,
        )
        self.assertEqual(summary["open_complete_modules"], 1)
        self.assertEqual(summary["missing_classpath_entries"], 3)
        self.assertEqual(summary["total_errors"], 2)


if __name__ == "__main__":
    unittest.main()
