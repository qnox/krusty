#!/usr/bin/env python3
"""Release title formatting and the actual workflow's publication call, without network."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/format-release-notes.py"
spec = importlib.util.spec_from_file_location("release_notes", SCRIPT)
notes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notes)
URL = "https://github.com/owner/repo/pull/12"


class ReleaseNotes(unittest.TestCase):
    def test_titles_are_literal_and_real_attribution_is_unchanged(self):
        for title in ["Keep @Target and @arbitrary-name", "nested `code` and ``code``",
                      "`both edges`", " space ", "<b>bold</b> & [link](url)",
                      "Unicode \u03bb **bold** \\ path"]:
            with self.subTest(title=title):
                raw = f"* {title} by @real-author in {URL}\n"
                formatted = notes.format_notes(raw)
                self.assertEqual(formatted,
                                 f"* {notes.literal_title(title)} by @real-author in {URL}\n")
        self.assertEqual(notes.literal_title("nested `x` and ``y``"),
                         "``` nested `x` and ``y`` ```")
        self.assertEqual(notes.literal_title("`edges`"), "`` `edges` ``")

    def test_title_cannot_spoof_the_final_author_suffix(self):
        title = f"Looks by @fake in {URL} but fixes @Target"
        self.assertEqual(notes.format_notes(f"* {title} by @real in {URL}\n"),
                         f"* ` {title} ` by @real in {URL}\n")

    def test_other_sections_and_line_endings_are_preserved(self):
        for ending in ["\n", "\r\n", ""]:
            prefix = "## What's Changed\n"
            suffix = "\n## New Contributors\n* @new-person made their first contribution in " + URL
            raw = prefix + f"* @Target by @dependabot[bot] in {URL}" + ending + suffix
            self.assertEqual(notes.format_notes(raw),
                             prefix + f"* ` @Target ` by @dependabot[bot] in {URL}" + ending + suffix)
        plain = "## Notes\n\n**Full Changelog**: https://github.com/owner/repo/compare/v1...v2\n"
        self.assertEqual(notes.format_notes(plain), plain)

    def test_cli_requires_a_generated_text_body(self):
        for data, code in [('{"body":"notes"}', 0), ('{}', 1),
                           ('{"body":null}', 1), ('not json', 1)]:
            result = subprocess.run(["python3", str(SCRIPT)], input=data, text=True,
                                    capture_output=True)
            self.assertEqual(result.returncode, code)
            if code == 0:
                self.assertEqual(result.stdout, "notes")
            else:
                self.assertEqual(result.stdout, "")

    def test_actual_publish_step_formats_notes_and_stops_on_generation_failure(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        step = workflow.split("      - name: publish\n", 1)[1].split("      - ", 1)[0]
        source = step.split("        run: |\n", 1)[1]
        source = "\n".join(line[10:] for line in source.splitlines()
                           if line.startswith("          "))
        for initial, status, query_fail, count, succeeds in [
            (None, 0, 0, 4, True), (None, 7, 0, 2, False),
            ("false", 0, 0, 2, True), ("true", 0, 0, 4, True),
            ("true", 0, 7, 2, False),
        ]:
            with self.subTest(initial=initial, status=status, query_fail=query_fail), tempfile.TemporaryDirectory() as temporary:
                work = Path(temporary)
                (work / "scripts").mkdir()
                (work / "bin").mkdir()
                (work / "dist").mkdir()
                (work / "dist/asset").write_text("validated artifact")
                if initial is not None:
                    (work / "state").write_text(initial)
                (work / "scripts/format-release-notes.py").write_bytes(SCRIPT.read_bytes())
                publisher = work / "scripts/release-publisher.sh"
                publisher.write_text("#!/bin/sh\nexit 0\n")
                publisher.chmod(0o755)
                gh = work / "bin/gh"
                gh.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
with open("calls", "a") as stream:
    stream.write(json.dumps(args) + "\\n")
if args == ["release", "view", "v1"]:
    sys.exit(0 if pathlib.Path("state").exists() else 1)
if args == ["release", "view", "v1", "--json", "isDraft", "--jq", ".isDraft"]:
    if int(os.environ["QUERY_FAIL"]):
        sys.exit(int(os.environ["QUERY_FAIL"]))
    print(pathlib.Path("state").read_text())
    sys.exit(0)
if args[:3] == ["api", "--method", "POST"]:
    assert args == ["api", "--method", "POST", "repos/owner/repo/releases/generate-notes",
                    "-f", "tag_name=v1", "-f", "target_commitish=abc123"]
    if int(os.environ["GENERATE_STATUS"]):
        sys.exit(int(os.environ["GENERATE_STATUS"]))
    print(json.dumps({"body": "* Keep @Target and `x` by @real in https://github.com/owner/repo/pull/12\\n"}))
elif args[:2] == ["release", "create"]:
    assert "--generate-notes" not in args
    assert args[args.index("--target") + 1] == "abc123"
    assert "--draft" in args and "--latest=false" in args
    assert args[-1] == "dist/asset"
    body = pathlib.Path(args[args.index("--notes-file") + 1]).read_text()
    assert body == "* `` Keep @Target and `x` `` by @real in https://github.com/owner/repo/pull/12\\n", body
    pathlib.Path("state").write_text("true")
elif args == ["release", "upload", "v1", "--clobber", "dist/asset"]:
    assert pathlib.Path("state").read_text() == "true"
elif args == ["release", "edit", "v1", "--draft=false", "--latest=false"]:
    pathlib.Path("state").write_text("false")
else:
    raise AssertionError(args)
''')
                gh.chmod(0o755)
                env = {**os.environ, "PATH": str(work / "bin") + ":" + os.environ["PATH"],
                       "GH_REPO": "owner/repo", "GITHUB_SHA": "abc123", "TAG": "v1",
                       "VER": "1", "RUNNER_TEMP": str(work), "GENERATE_STATUS": str(status),
                       "QUERY_FAIL": str(query_fail)}
                result = subprocess.run(["bash", "-c", source], cwd=work, env=env,
                                        text=True, capture_output=True)
                calls = [json.loads(line) for line in (work / "calls").read_text().splitlines()]
                self.assertEqual(result.stdout, "")
                self.assertFalse((work / "krusty-release-notes.md").exists())
                self.assertEqual(len(calls), count, calls)
                if succeeds:
                    self.assertEqual(result.returncode, 0, result.stderr)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(any(call[:2] in [["release", "create"],
                                                     ["release", "upload"],
                                                     ["release", "edit"]] for call in calls))


if __name__ == "__main__":
    unittest.main()
