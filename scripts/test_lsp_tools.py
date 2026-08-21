#!/usr/bin/env python3
"""Focused unit coverage for the standalone LSP audit tools."""

import contextlib
import importlib.util
import io
import json
import pathlib
import queue
import struct
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT_DIR = pathlib.Path(__file__).resolve().parent


def load_script(module_name, filename):
    spec = importlib.util.spec_from_file_location(module_name, SCRIPT_DIR / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


LSP_DIFF = load_script("krusty_lsp_diff", "lsp-diff.py")
WORDING_AUDIT = load_script("krusty_lsp_wording_audit", "lsp-wording-audit.py")


def diagnostic(message, start, end):
    return {
        "message": message,
        "range": {
            "start": {"line": start[0], "character": start[1]},
            "end": {"line": end[0], "character": end[1]},
        },
    }


def run_lsp_diff(directory, fake_lsp, report_path=None):
    argv = [
        "lsp-diff.py",
        directory,
        "--reference",
        "reference-server",
        "--krusty",
        "krusty-server",
        "--limit",
        "1",
    ]
    if report_path is not None:
        argv.extend(("--json", str(report_path)))
    stdout = io.StringIO()
    stderr = io.StringIO()
    with (
        mock.patch.object(LSP_DIFF, "Lsp", fake_lsp),
        mock.patch.object(sys, "argv", argv),
        contextlib.redirect_stdout(stdout),
        contextlib.redirect_stderr(stderr),
    ):
        result = LSP_DIFF.main()
    return result, stdout.getvalue(), stderr.getvalue()


def fake_lsp(events, initialize_error=None, timeout_server=None):
    class FakeLsp:
        def __init__(self, _argv, _root, name):
            self.name = name
            events.append(("start", name))

        def initialize(self, _root, _timeout):
            events.append(("initialize", self.name))
            if initialize_error is not None:
                raise initialize_error

        def diagnostics_for(self, _path, _timeout):
            events.append(("diagnostics", self.name))
            if self.name == timeout_server:
                raise TimeoutError(f"{self.name}: textDocument/diagnostic timed out")
            return []

        def shutdown(self):
            events.append(("shutdown", self.name))

    return FakeLsp


class LspDiffTests(unittest.TestCase):
    def test_main_stops_reference_before_starting_krusty(self):
        events = []

        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "Main.kt").write_text("fun value() = 1", encoding="utf-8")
            result, stdout, _stderr = run_lsp_diff(directory, fake_lsp(events))

        self.assertEqual(result, 0)
        self.assertEqual(
            events,
            [
                ("start", "reference"),
                ("initialize", "reference"),
                ("diagnostics", "reference"),
                ("shutdown", "reference"),
                ("start", "krusty"),
                ("initialize", "krusty"),
                ("diagnostics", "krusty"),
                ("shutdown", "krusty"),
            ],
        )
        self.assertEqual(
            stdout,
            "lsp-diff: matched=0 extra=0 missing=0 wording=0 moved=0 unanswered=0 files=1\n",
        )

    def test_main_stops_reference_when_initialize_fails(self):
        events = []

        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "Main.kt").write_text("fun value() = 1", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "^reference initialize failed$"):
                run_lsp_diff(
                    directory,
                    fake_lsp(events, initialize_error=RuntimeError("reference initialize failed")),
                )

        self.assertEqual(
            events,
            [
                ("start", "reference"),
                ("initialize", "reference"),
                ("shutdown", "reference"),
            ],
        )

    def test_main_preserves_the_exact_timeout_report(self):
        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "Main.kt").write_text("fun value() = 1", encoding="utf-8")
            report_path = pathlib.Path(directory, "report.json")
            result, stdout, stderr = run_lsp_diff(
                directory,
                fake_lsp([], timeout_server="reference"),
                report_path,
            )

            self.assertEqual(result, 0)
            self.assertEqual(
                json.loads(report_path.read_text(encoding="utf-8")),
                {
                    "root": directory,
                    "files": {
                        "Main.kt": {
                            "timeout": "reference: textDocument/diagnostic timed out"
                        }
                    },
                    "totals": {
                        "matched": 0,
                        "extra": 0,
                        "missing": 0,
                        "wording": 0,
                        "moved": 0,
                        "unanswered": 1,
                        "files": 0,
                    },
                },
            )

        self.assertEqual(
            stdout,
            "lsp-diff: matched=0 extra=0 missing=0 wording=0 moved=0 unanswered=1 files=0\n",
        )
        self.assertEqual(
            stderr,
            "[lsp-diff] reference: initializing reference-server\n"
            "[lsp-diff] krusty: initializing krusty-server\n"
            "[lsp-diff] Main.kt: reference: textDocument/diagnostic timed out\n",
        )

    def test_same_message_with_a_different_end_is_moved(self):
        result = LSP_DIFF.compare(
            [diagnostic("Same message", (1, 2), (1, 5))],
            [diagnostic("Same message", (1, 2), (1, 6))],
        )
        self.assertEqual(
            result,
            {
                "matched": [],
                "extra": [],
                "missing": [],
                "wording": [],
                "moved": [
                    {
                        "message": "Same message",
                        "reference_at": (1, 2, 1, 5),
                        "krusty_at": (1, 2, 1, 6),
                    }
                ],
            },
        )

    def test_same_full_range_with_a_different_message_is_wording(self):
        result = LSP_DIFF.compare(
            [diagnostic("Reference", (1, 2), (1, 5))],
            [diagnostic("Krusty", (1, 2), (1, 5))],
        )
        self.assertEqual(
            result,
            {
                "matched": [],
                "extra": [],
                "missing": [],
                "wording": [
                    {
                        "at": (1, 2, 1, 5),
                        "reference": "Reference",
                        "krusty": "Krusty",
                    }
                ],
                "moved": [],
            },
        )

    def test_timed_out_request_restores_deferred_notifications(self):
        client = LSP_DIFF.Lsp.__new__(LSP_DIFF.Lsp)
        client.name = "test"
        client.next_id = 1
        client.inbox = queue.Queue()
        client.send = lambda _message: None
        notification = {"jsonrpc": "2.0", "method": "window/logMessage", "params": {}}
        client.inbox.put(notification)

        with self.assertRaises(TimeoutError):
            client.request("test/request", {}, 0.01)

        self.assertEqual(client.inbox.get_nowait(), notification)

    def test_kotlin_file_walk_is_stable_and_skips_build_trees(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for relative in ("z/Z.kt", "a/A.kt", "build/Hidden.kt", "target/Generated.kt"):
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fun value() = 1", encoding="utf-8")
            found = [pathlib.Path(path).relative_to(root).as_posix()
                     for path in LSP_DIFF.find_kotlin_files(root, 10)]
        self.assertEqual(found, ["a/A.kt", "z/Z.kt"])


class WordingAuditTests(unittest.TestCase):
    def test_constant_pool_utf8_entry_is_read(self):
        message = b"Unresolved reference '{0}'."
        class_bytes = (
            b"\xca\xfe\xba\xbe\x00\x00\x00\x34"
            + struct.pack(">H", 2)
            + b"\x01"
            + struct.pack(">H", len(message))
            + message
        )
        self.assertEqual(list(WORDING_AUDIT.constant_pool_strings(class_bytes)),
                         [message.decode()])

    def test_balanced_call_ignores_parentheses_inside_strings(self):
        source = '.error(span, format!("ordinary )", r#"raw )"#)) trailing'
        call = WORDING_AUDIT.balanced_call(source, source.index("("))
        self.assertEqual(call, '(span, format!("ordinary )", r#"raw )"#))')


if __name__ == "__main__":
    unittest.main()
