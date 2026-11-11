#!/usr/bin/env python3
"""The top-project list: compile rows stay buildable, and the LSP set adds the two large roots."""

import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import projects  # noqa: E402

HERE = Path(__file__).resolve().parent


class Projects(unittest.TestCase):
    def test_a_lane_token_is_only_the_last_field(self):
        text = "\n".join(
            [
                "# comment",
                "ok\thttps://example.test/ok.git\t" + "a" * 40 + "\t.\t21\tgradle.properties:kotlin",
                "android\thttps://example.test/android\t" + "b" * 40 + "\t.\t21\tlibs:kotlin\tdemoDebug",
                "idea\thttps://example.test/idea\t" + "c" * 40 + "\t.\t21\t-\t-\tlsp",
                "both\thttps://example.test/both\t" + "d" * 40 + "\t.\t21\t-\tcompile,lsp",
            ]
        )
        rows = {row["name"]: row["lane"] for row in projects.parse(text)}
        self.assertEqual(rows, {"ok": "compile,lsp", "android": "compile,lsp", "idea": "lsp", "both": "compile,lsp"})
        self.assertEqual([row["name"] for row in projects.select(projects.parse(text), "compile")], ["ok", "android", "both"])
        self.assertEqual([row["name"] for row in projects.select(projects.parse(text), "lsp")], ["ok", "android", "idea", "both"])

    def test_a_short_commit_is_rejected(self):
        with self.assertRaises(ValueError):
            projects.parse("p\thttps://example.test/p\tnot-a-sha\t.\t21\t-\n")

    def test_the_checked_in_list_builds_the_existing_projects_and_measures_idea_and_kotlin(self):
        rows = projects.load(HERE / "projects.tsv")
        names = [row["name"] for row in rows]
        self.assertIn("okhttp", names)
        self.assertIn("nowinandroid", names)
        self.assertIn("intellij-community", names)
        self.assertIn("kotlin", names)
        compile_names = [row["name"] for row in projects.select(rows, "compile")]
        lsp_names = [row["name"] for row in projects.select(rows, "lsp")]
        self.assertNotIn("intellij-community", compile_names)
        self.assertNotIn("kotlin", compile_names)
        self.assertIn("okhttp", compile_names)
        self.assertIn("nowinandroid", compile_names)
        for name in names:
            self.assertIn(name, lsp_names)

    def test_the_build_scripts_refuse_a_language_server_row_before_checkout(self):
        row = "idea\thttps://example.test/idea\t" + "c" * 40 + "\t.\t21\t-\t-\tlsp\n"
        for script in ("run.sh", "bench.sh"):
            with self.subTest(script), tempfile.TemporaryDirectory() as work:
                projects_file = Path(work) / "projects.tsv"
                projects_file.write_text(row)
                env = dict(os.environ, CORPUS_PROJECTS=str(projects_file))
                result = subprocess.run(
                    ["bash", str(HERE / script), "idea", f"{work}/corpus", "/nonexistent/krusty",
                     "/nonexistent/repository", "0"],
                    env=env, capture_output=True, text=True,
                )
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("language-server metric set", result.stderr)
                self.assertFalse((Path(work) / "corpus" / "src").exists())

    def test_compile_lsp_on_an_android_row_exports_only_the_variant(self):
        # The lane is the last field. Leaving it for `read` stores `demoDebug` and
        # `compile,lsp` together in CORPUS_ANDROID_VARIANT.
        for script in ("run.sh", "bench.sh"):
            with self.subTest(script), tempfile.TemporaryDirectory() as work:
                repo = Path(work) / "upstream"
                repo.mkdir()
                sentinel = Path(work) / "variant"
                quoted = shlex.quote(str(sentinel))
                gradlew = repo / "gradlew"
                gradlew.write_text(
                    "#!/bin/sh\n"
                    "if [ \"${CORPUS_ANDROID_VARIANT+set}\" = set ]; then\n"
                    f"  printf '%s\\n' \"$CORPUS_ANDROID_VARIANT\" >> {quoted}\n"
                    "else\n"
                    f"  printf 'unset\\n' >> {quoted}\n"
                    "fi\n"
                    "exit 1\n"
                )
                gradlew.chmod(0o755)
                git = ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t"]
                subprocess.run(["git", "init", "-q", str(repo)], check=True)
                subprocess.run(git + ["add", "."], check=True)
                subprocess.run(git + ["commit", "-qm", "init"], check=True)
                commit = subprocess.run(
                    git + ["rev-parse", "HEAD"], check=True, capture_output=True, text=True
                ).stdout.strip()
                projects_file = Path(work) / "projects.tsv"
                projects_file.write_text(
                    f"android\tfile://{repo}\t{commit}\t.\t21\t-\tdemoDebug\tcompile,lsp\n"
                )
                env = dict(os.environ, CORPUS_PROJECTS=str(projects_file))
                env.pop("CORPUS_ANDROID_VARIANT", None)
                env.setdefault("JAVA_HOME", "/usr")
                result = subprocess.run(
                    ["bash", str(HERE / script), "android", f"{work}/corpus",
                     "/nonexistent/krusty", "/nonexistent/repository", "0"],
                    env=env, capture_output=True, text=True,
                )
                self.assertNotIn("language-server metric set", result.stderr)
                recorded = sentinel.read_text().splitlines() if sentinel.exists() else []
                self.assertTrue(recorded, result.stderr)
                self.assertEqual(set(recorded), {"demoDebug"}, result.stderr)


if __name__ == "__main__":
    unittest.main()
