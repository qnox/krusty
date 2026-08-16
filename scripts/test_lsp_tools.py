#!/usr/bin/env python3
"""Focused unit coverage for the standalone LSP audit tools."""

import importlib.util
import pathlib
import queue
import struct
import tempfile
import unittest


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


class LspDiffTests(unittest.TestCase):
    def test_same_message_with_a_different_end_is_moved(self):
        result = LSP_DIFF.compare(
            [diagnostic("Same message", (1, 2), (1, 5))],
            [diagnostic("Same message", (1, 2), (1, 6))],
        )
        self.assertEqual(result["matched"], [])
        self.assertEqual(len(result["moved"]), 1)

    def test_same_full_range_with_a_different_message_is_wording(self):
        result = LSP_DIFF.compare(
            [diagnostic("Reference", (1, 2), (1, 5))],
            [diagnostic("Krusty", (1, 2), (1, 5))],
        )
        self.assertEqual(result["matched"], [])
        self.assertEqual(len(result["wording"]), 1)

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
