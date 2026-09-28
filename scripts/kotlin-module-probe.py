#!/usr/bin/env python3
"""Compile one JVM module from a Gradle project model with krusty, and its Java with javac.

JetBrains/kotlin builds each module twice over: a published bootstrap compiler generates the
FIR tree and the stdlib builtins, then kotlinc compiles the module's own sources. This probe
is only the second step. It reads the project model `krusty-lsp model` already resolved
(source roots, classpath, friend paths, free compiler args) and:

  * sends `.kt` and `.java` to krusty, so Kotlin sees same-module Java headers;
  * sends `.java` to javac afterwards, with krusty's class directory on the classpath;
  * compiles a generated root only when that directory is already on disk;
  * leaves `:kotlin-stdlib` to the published stdlib jar unless `--include-stdlib` is set.

    scripts/kotlin-module-probe.py --model model.json --list
    scripts/kotlin-module-probe.py --model model.json --module ':compiler:util:main' --dry-run
    scripts/kotlin-module-probe.py --self-test
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

# The JVM stdlib is compiled from source only after the compiler modules that depend on the
# published jar already build. `:kotlin-stdlib-jdk8` and the other platform jars are separate
# modules and are not this one.
STDLIB_PROJECT = ":kotlin-stdlib"

# Flags the probe sets from structured model fields. Forwarding them again from
# `kotlinc_args` would hand krusty two outputs or two classpaths.
OWNED_FLAGS = {
    "-d",
    "-cp",
    "-classpath",
    "-class-path",
    "-module-name",
    "-jdk-home",
    "-jvm-target",
    "-Xfriend-paths",
}
OWNED_PREFIXES = tuple(f"{flag}=" for flag in OWNED_FLAGS)


def project_path(module_id: str) -> str:
    """`:compiler:util:main` is project `:compiler:util`, source set `main`."""
    if ":" not in module_id:
        return module_id
    project, _sep, _source_set = module_id.rpartition(":")
    return project


def is_stdlib_module(module: dict) -> bool:
    return project_path(module.get("id") or "") == STDLIB_PROJECT


def classpath_join(entries: list[str]) -> str:
    return os.pathsep.join(entries)


def collect_sources(module: dict, *, include_tests: bool) -> tuple[list[Path], list[Path]]:
    """Kotlin and Java files under the module's source roots.

    A root that is not on disk is skipped. That is how a generated root looks before the
    bootstrap compiler has written it; this probe does not run the generator.
    """
    kotlin: list[Path] = []
    java: list[Path] = []
    for root in module.get("source_roots", []):
        if root.get("kind") == "test" and not include_tests:
            continue
        directory = Path(root["path"])
        if not directory.is_dir():
            continue
        for path in sorted(directory.rglob("*")):
            if not path.is_file():
                continue
            if path.suffix == ".kt":
                kotlin.append(path)
            elif path.suffix == ".java":
                java.append(path)
    return kotlin, java


def forwarded_args(kotlinc_args: list[str]) -> list[str]:
    """Free compiler args, minus the output and classpath flags the probe owns."""
    forwarded: list[str] = []
    index = 0
    while index < len(kotlinc_args):
        token = kotlinc_args[index]
        if token in OWNED_FLAGS:
            index += 2
            continue
        if token.startswith(OWNED_PREFIXES):
            index += 1
            continue
        forwarded.append(token)
        index += 1
    return forwarded


def has_stdlib_jar(entries: list[str]) -> bool:
    for entry in entries:
        name = Path(entry).name
        if name == "kotlin-stdlib.jar":
            return True
        if name.startswith("kotlin-stdlib-") and name.endswith(".jar"):
            version = name[len("kotlin-stdlib-") : -len(".jar")]
            if version[:1].isdigit():
                return True
    return False


def plan_module(
    module: dict,
    *,
    out_dir: Path,
    stdlib: str | None,
    jdk_home: str | None,
    include_stdlib: bool,
    include_tests: bool,
    krusty: str,
    javac: str,
) -> dict | None:
    """One module's javac and krusty invocations, or None when the module is skipped."""
    module_id = module.get("id") or module.get("name") or "module"
    if is_stdlib_module(module) and not include_stdlib:
        return {
            "id": module_id,
            "status": "skipped",
            "reason": "stdlib stays the published jar; pass --include-stdlib to compile it",
        }
    kotlin, java = collect_sources(module, include_tests=include_tests)
    if not kotlin and not java:
        return {
            "id": module_id,
            "status": "skipped",
            "reason": "no Kotlin or Java sources on disk",
        }

    classes = out_dir / "classes"
    classpath: list[str] = [str(path) for path in module.get("classpath", [])]
    if stdlib and not has_stdlib_jar(classpath):
        classpath.append(stdlib)
    friends = [str(path) for path in module.get("friend_paths", [])]
    args = list(module.get("kotlinc_args") or [])

    krusty_cmd = [krusty, "-d", str(classes), "-module-name", _module_name(module)]
    if jdk_home:
        krusty_cmd += ["-jdk-home", jdk_home]
    if classpath:
        krusty_cmd += ["-classpath", classpath_join(classpath)]
    if friends:
        krusty_cmd.append("-Xfriend-paths=" + classpath_join(friends))
    jvm_target = module.get("jvm_target")
    if jvm_target:
        krusty_cmd += ["-jvm-target", str(jvm_target)]
    krusty_cmd += forwarded_args(args)
    krusty_cmd += [str(path) for path in kotlin]
    krusty_cmd += [str(path) for path in java]

    javac_cmd = None
    if java:
        javac_cp = [str(classes), *classpath]
        javac_cmd = [javac, "-d", str(classes), "-classpath", classpath_join(javac_cp)]
        javac_cmd += [str(path) for path in java]

    return {
        "id": module_id,
        "status": "planned",
        "kotlin": len(kotlin),
        "java": len(java),
        "classes": str(classes),
        "krusty": krusty_cmd,
        "javac": javac_cmd,
    }


def _module_name(module: dict) -> str:
    name = module.get("name") or module.get("id") or "main"
    return name.replace(":", "_")


def select_modules(model: dict, wanted: list[str]) -> list[dict]:
    modules = model.get("modules", [])
    if not wanted:
        return modules
    by_id = {module.get("id"): module for module in modules}
    by_name = {module.get("name"): module for module in modules}
    chosen = []
    for token in wanted:
        module = by_id.get(token) or by_name.get(token)
        if module is None:
            known = ", ".join(str(module.get("id")) for module in modules)
            raise SystemExit(f"no module {token!r} in the model; known: {known}")
        chosen.append(module)
    return chosen


def run_plan(planned: dict) -> dict:
    classes = Path(planned["classes"])
    classes.mkdir(parents=True, exist_ok=True)
    krusty = subprocess.run(planned["krusty"], capture_output=True, text=True)
    result = {
        "id": planned["id"],
        "status": "built" if krusty.returncode == 0 else "failed",
        "krusty_exit": krusty.returncode,
        "output": (krusty.stdout + krusty.stderr)[:4000],
    }
    if krusty.returncode != 0 or not planned["javac"]:
        return result
    javac = subprocess.run(planned["javac"], capture_output=True, text=True)
    result["javac_exit"] = javac.returncode
    if javac.returncode != 0:
        result["status"] = "failed"
        result["output"] = (result["output"] + javac.stdout + javac.stderr)[:4000]
    return result


def self_test() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        source = root / "src"
        generated = root / "build" / "generated"
        source.mkdir()
        (source / "Util.kt").write_text("fun util() = 1\n")
        (source / "Util.java").write_text("class Util {}\n")
        (source / "Skip.kts").write_text("println(1)\n")
        test_root = root / "test"
        test_root.mkdir()
        (test_root / "UtilTest.kt").write_text("fun test() = 1\n")
        # Absent on purpose: the bootstrap compiler has not generated it yet.
        missing_generated = root / "build" / "fir"
        module = {
            "id": ":compiler:util:main",
            "name": "compiler:util:main",
            "jvm_target": "21",
            "classpath": [str(root / "deps" / "annotations.jar")],
            "friend_paths": [str(root / "out" / "common")],
            "kotlinc_args": [
                "-Xcontext-parameters",
                "-Xexplicit-backing-fields",
                "-Xname-based-destructuring=complete",
                "-jvm-default=no-compatibility",
                "-opt-in=kotlin.RequiresOptIn",
                "-d",
                "/ignored",
                "-classpath",
                "/also-ignored",
                "-d=/ignored-equals",
                "-classpath=/also-ignored-equals",
                "-module-name=ignored",
                "-jdk-home=/ignored-jdk",
                "-jvm-target",
                "17",
                "-jvm-target=11",
                "-Xfriend-paths",
                "/ignored-friend",
                "-Xfriend-paths=/elsewhere",
            ],
            "source_roots": [
                {"path": str(source), "kind": "source", "generated": False},
                {"path": str(generated), "kind": "source", "generated": True},
                {"path": str(missing_generated), "kind": "source", "generated": True},
                {"path": str(test_root), "kind": "test", "generated": False},
            ],
        }
        generated.mkdir(parents=True)
        (generated / "Tree.kt").write_text("fun tree() = 1\n")

        kotlin, java = collect_sources(module, include_tests=False)
        assert [path.name for path in kotlin] == ["Util.kt", "Tree.kt"], kotlin
        assert [path.name for path in java] == ["Util.java"], java
        with_tests, _ = collect_sources(module, include_tests=True)
        assert "UtilTest.kt" in [path.name for path in with_tests]

        stdlib = {
            "id": ":kotlin-stdlib:main",
            "name": "kotlin-stdlib:main",
            "source_roots": [{"path": str(source), "kind": "source", "generated": False}],
        }
        assert is_stdlib_module(stdlib)
        assert not is_stdlib_module(module)
        assert not is_stdlib_module({"id": ":kotlin-stdlib-jdk8:main"})

        skipped = plan_module(
            stdlib,
            out_dir=root / "stdlib-out",
            stdlib=str(root / "kotlin-stdlib.jar"),
            jdk_home=None,
            include_stdlib=False,
            include_tests=False,
            krusty="krusty",
            javac="javac",
        )
        assert skipped is not None and skipped["status"] == "skipped", skipped

        planned = plan_module(
            module,
            out_dir=root / "out",
            stdlib=str(root / "kotlin-stdlib-2.4.20.jar"),
            jdk_home="/jdk",
            include_stdlib=False,
            include_tests=False,
            krusty="krusty",
            javac="javac",
        )
        assert planned is not None and planned["status"] == "planned", planned
        assert planned["kotlin"] == 2 and planned["java"] == 1
        command = planned["krusty"]
        assert command[:4] == ["krusty", "-d", str(root / "out" / "classes"), "-module-name"]
        assert "-jdk-home" in command and "/jdk" in command
        assert "-jvm-target" in command and "21" in command
        assert command.count("-jvm-target") == 1
        assert "-Xcontext-parameters" in command
        assert "-Xexplicit-backing-fields" in command
        assert "-Xname-based-destructuring=complete" in command
        assert "-jvm-default=no-compatibility" in command
        assert "-opt-in=kotlin.RequiresOptIn" in command
        assert "/ignored" not in command
        assert "/also-ignored" not in command
        assert not any("ignored-equals" in token for token in command)
        assert "-module-name=ignored" not in command
        assert "-jdk-home=/ignored-jdk" not in command
        assert "-jvm-target=11" not in command
        assert "17" not in command
        assert "/ignored-friend" not in command
        assert "-Xfriend-paths=/elsewhere" not in command
        friend = next(token for token in command if token.startswith("-Xfriend-paths="))
        assert str(root / "out" / "common") in friend
        classpath = command[command.index("-classpath") + 1]
        assert "kotlin-stdlib-2.4.20.jar" in classpath
        assert "annotations.jar" in classpath
        assert str(source / "Util.kt") in command
        assert str(generated / "Tree.kt") in command
        assert command[-1] == str(source / "Util.java")
        assert "Skip.kts" not in " ".join(command)
        assert "UtilTest.kt" not in " ".join(command)
        assert planned["javac"] is not None
        assert planned["javac"][0] == "javac"
        assert str(root / "out" / "classes") in planned["javac"]
        assert str(source / "Util.java") in planned["javac"]

        assert has_stdlib_jar([str(root / "kotlin-stdlib.jar")])
        assert has_stdlib_jar([str(root / "kotlin-stdlib-2.4.20.jar")])
        assert not has_stdlib_jar([str(root / "kotlin-stdlib-jdk8.jar")])
        assert not has_stdlib_jar([str(root / "kotlin-stdlib-common-2.4.20.jar")])
        assert not has_stdlib_jar([str(root / "2.4.20.jar")])

        already = plan_module(
            {
                **module,
                "classpath": [str(root / "kotlin-stdlib.jar")],
            },
            out_dir=root / "out2",
            stdlib=str(root / "other-stdlib.jar"),
            jdk_home=None,
            include_stdlib=False,
            include_tests=False,
            krusty="krusty",
            javac="javac",
        )
        classpath = already["krusty"][already["krusty"].index("-classpath") + 1]
        assert "other-stdlib.jar" not in classpath
        assert "kotlin-stdlib.jar" in classpath

    print("kotlin-module-probe: self-test ok")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--model", help="JSON from `krusty-lsp model <root>`")
    parser.add_argument("--module", action="append", default=[], help="module id or name; repeatable")
    parser.add_argument("--list", action="store_true", help="print module ids and source counts")
    parser.add_argument("--dry-run", action="store_true", help="print the planned commands as JSON")
    parser.add_argument("--include-stdlib", action="store_true", help="compile :kotlin-stdlib from source")
    parser.add_argument("--include-tests", action="store_true", help="compile test source roots too")
    parser.add_argument("--out", default="", help="class output root (default: a temp directory)")
    parser.add_argument("--stdlib", default=os.environ.get("KRUSTY_KOTLIN_STDLIB", ""), help="published kotlin-stdlib jar")
    parser.add_argument("--krusty", default="krusty")
    parser.add_argument("--javac", default="javac")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        self_test()
        return
    if not args.model:
        raise SystemExit("--model is required (or pass --self-test)")

    model = json.loads(Path(args.model).read_text())
    chosen = select_modules(model, args.module)
    if args.list:
        for module in chosen:
            kotlin, java = collect_sources(module, include_tests=args.include_tests)
            mark = "stdlib" if is_stdlib_module(module) else "module"
            print(f"{module.get('id')}\t{mark}\t{len(kotlin)} kt\t{len(java)} java")
        return

    if not args.module:
        raise SystemExit("pass --module <id> (or --list)")

    out_root = Path(args.out) if args.out else Path(tempfile.mkdtemp(prefix="krusty-kotlin-module-"))
    jdk_home = model.get("jdk_home") or os.environ.get("JAVA_HOME")
    rows = []
    for module in chosen:
        module_out = out_root / _module_name(module)
        planned = plan_module(
            module,
            out_dir=module_out,
            stdlib=args.stdlib or None,
            jdk_home=jdk_home,
            include_stdlib=args.include_stdlib,
            include_tests=args.include_tests,
            krusty=args.krusty,
            javac=args.javac,
        )
        if planned is None:
            continue
        if args.dry_run or planned["status"] == "skipped":
            rows.append(planned)
            continue
        rows.append(run_plan(planned))

    json.dump(rows, sys.stdout, indent=1)
    sys.stdout.write("\n")
    if any(row.get("status") == "failed" for row in rows):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
