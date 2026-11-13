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
        self.assertIsNone(summary["definition_query_us"])
        self.assertIsNone(summary["references_query_us"])
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

    def test_navigation_queries_are_not_copied_from_the_analysis_time(self):
        summary = SCAN.summarize(
            [
                report(elapsed_ms=4000, definition_query_us=12, references_query_us=40),
                report(
                    module="lib",
                    elapsed_ms=9,
                    definition_query_us=None,
                    references_query_us=None,
                ),
            ],
            top=5,
        )
        self.assertIsNone(summary["parse_ms"])
        self.assertIsNone(summary["type_info_ms"])
        self.assertEqual(summary["definition_query_us"], 12)
        self.assertEqual(summary["references_query_us"], 40)
        text = SCAN.render_text(summary)
        navigation = next(line for line in text.splitlines() if line.startswith("navigation"))
        self.assertIn("parse        not measured", text)
        self.assertIn("definition 12 µs, references 40 µs", navigation)
        self.assertNotIn("4000", navigation)


if __name__ == "__main__":
    unittest.main()
