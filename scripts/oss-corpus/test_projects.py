#!/usr/bin/env python3
"""The top-project list: compile rows stay buildable, the LSP set adds intellij-community, and the
build scripts hand Gradle a project's target set and every installed JDK."""

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

    def test_the_checked_in_list_builds_and_measures_idea_and_kotlin(self):
        rows = projects.load(HERE / "projects.tsv")
        names = [row["name"] for row in rows]
        self.assertIn("okhttp", names)
        self.assertIn("nowinandroid", names)
        self.assertIn("intellij-community", names)
        self.assertIn("kotlin", names)
        compile_names = [row["name"] for row in projects.select(rows, "compile")]
        lsp_names = [row["name"] for row in projects.select(rows, "lsp")]
        self.assertIn("intellij-community", compile_names)
        self.assertIn("kotlin", compile_names)
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

    def test_kotlin_builds_a_listed_target_set_with_a_released_bootstrap_compiler(self):
        # kotlin compiles itself with its bootstrap compiler, which the pin moves to the newest
        # release in kotlin-versions; the target set keeps the build within a runner's budget.
        rows = {line.split("\t")[0]: line.split("\t") for line in (HERE / "projects.tsv").read_text().splitlines()
                if line and not line.startswith("#")}
        self.assertEqual(rows["kotlin"][5], "gradle.properties:bootstrap.kotlin.version")
        targets = [line.split("#")[0].strip() for line in (HERE / "gradle/kotlin.targets").read_text().splitlines()]
        targets = [target for target in targets if target]
        self.assertTrue(targets)
        for target in targets:
            self.assertRegex(target, r"^(:[A-Za-z0-9._-]+)+$")


class BuildSetup(unittest.TestCase):
    """What run.sh and bench.sh hand Gradle: a project's target set and every installed JDK."""

    def run_script(self, script, work, targets):
        repo = Path(work) / "upstream"
        repo.mkdir()
        record = shlex.quote(str(Path(work) / "gradle-calls"))
        gradlew = repo / "gradlew"
        gradlew.write_text(
            "#!/bin/sh\n"
            f"printf '%s|%s\\n' \"${{CORPUS_TARGETS-unset}}\" \"$*\" >> {record}\n"
            "exit 1\n"
        )
        gradlew.chmod(0o755)
        git = ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t"]
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        subprocess.run(git + ["add", "."], check=True)
        subprocess.run(git + ["commit", "-qm", "init"], check=True)
        commit = subprocess.run(git + ["rev-parse", "HEAD"], check=True, capture_output=True, text=True).stdout.strip()
        projects_file = Path(work) / "projects.tsv"
        projects_file.write_text(f"p\tfile://{repo}\t{commit}\t.\t21\t-\n")
        targets_dir = Path(work) / "targets"
        targets_dir.mkdir()
        if targets is not None:
            (targets_dir / "p.targets").write_text(targets)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith("JAVA_HOME_") and key != "CORPUS_TARGETS"}
        env.update(CORPUS_PROJECTS=str(projects_file), CORPUS_TARGETS_DIR=str(targets_dir),
                   JAVA_HOME_17_X64="/jdk/17", JAVA_HOME_8_X64="/jdk/8", JAVA_HOME_11_X64="/jdk/11",
                   JAVA_HOME_17_arm64="/jdk/other", CORPUS_TARGETS="/inherited")
        env.setdefault("JAVA_HOME", "/usr")
        subprocess.run(["bash", str(HERE / script), "p", f"{work}/corpus", "/nonexistent/krusty",
                        "/nonexistent/repository", "0"], env=env, capture_output=True, text=True)
        calls = (Path(work) / "gradle-calls").read_text().splitlines()
        self.assertTrue(calls)
        return [call.split("|", 1) for call in calls], targets_dir / "p.targets"

    def test_every_gradle_call_gets_the_target_set_and_the_installed_jdks(self):
        for script in ("run.sh", "bench.sh"):
            with self.subTest(script), tempfile.TemporaryDirectory() as work:
                calls, targets = self.run_script(script, work, ":a:compileKotlin\n")
                for target_set, args in calls:
                    self.assertEqual(target_set, str(targets))
                    self.assertIn("-Porg.gradle.java.installations.paths=/jdk/8,/jdk/11,/jdk/17", args.split(" "))

    def test_a_project_without_a_target_set_drops_an_inherited_one(self):
        for script in ("run.sh", "bench.sh"):
            with self.subTest(script), tempfile.TemporaryDirectory() as work:
                calls, _ = self.run_script(script, work, None)
                self.assertEqual({target_set for target_set, _ in calls}, {"unset"})


if __name__ == "__main__":
    unittest.main()
