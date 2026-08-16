#!/usr/bin/env python3
"""Build real project modules through krusty's Bazel persistent worker, and report what blocks it.

This measures the `krusty_jvm_library` path end to end WITHOUT bazel: the worker speaks
jvm-inc-builder's argument surface over line-delimited JSON on stdin/stdout, so it can be driven
directly. That matters because the interesting failures are krusty's, and a bazel install would only
add a layer between the probe and them.

What it does:

  * takes the project model from `krusty-lsp model` (the same model the parity harness uses), so
    module sources, classpaths and per-module kotlinc args are the project's real ones;
  * selects modules whose FULL transitive dependency closure is Kotlin-only — krusty compiles no
    Java, so a closure containing Java sources can never be built from source alone and its
    "unresolved reference" failures would say nothing about krusty;
  * builds them in dependency order through ONE worker process, feeding each produced jar to its
    dependents' `--cp`, which is what upstream `krusty_jvm_library` targets supply under bazel;
  * classifies failures on the first line that is not the worker's inert-option note.

    scripts/bazel-worker-probe.py                       # probe, print the summary
    scripts/bazel-worker-probe.py --limit 20 --json out.json

Translating the model's kotlinc args into the worker vocabulary reproduces what the Starlark rule
emits, including intellij's own legacy `-Xjvm-default=all` -> `no-compatibility` normalization
(build/compiler-options.bzl). Skipping that normalization makes every affected module look like a
krusty refusal when the rule would never have sent the legacy spelling.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from collections import Counter
from pathlib import Path

# The worker reports options it understood but that change no emitted byte. That note is a SUCCESS
# channel and is printed above any real error, so classifying on the first output line attributes
# every failure to it.
INERT_PREFIX = "krusty: no effect on output:"

DEFAULT_ROOT = os.environ.get("KRUSTY_IJ_ROOT", str(Path.home() / "external-projects" / "intellij-community"))


def has_modules(home: str) -> bool:
    """A real JDK, not a stub: without one every `java.*` reference resolves to nothing."""
    return bool(home) and (Path(home) / "lib" / "modules").exists()


def java_home() -> str | None:
    home = os.environ.get("JAVA_HOME", "")
    return home if has_modules(home) else None


def worker_args(raw: list[str]) -> list[str]:
    """Translate a module's kotlinc args into the jvm-inc-builder vocabulary the rule emits."""
    out: list[str] = []
    index = 0
    while index < len(raw):
        token = raw[index]
        if token in ("-api-version", "-language-version") and index + 1 < len(raw):
            flag = "--api_version" if token == "-api-version" else "--language_version"
            out += [flag, raw[index + 1]]
            index += 2
            continue
        if token == "-progressive":
            out += ["--progressive"]
        elif token.startswith("-Xjvm-default="):
            # intellij normalizes the legacy spelling in build/compiler-options.bzl before the
            # builder sees it; the worker deliberately accepts only the modern names.
            legacy = token.split("=", 1)[1]
            out += ["--jvm_default", {"all": "no-compatibility", "all-compatibility": "enable"}.get(legacy, legacy)]
        elif token.startswith("-opt-in="):
            out += ["--opt_in", token.split("=", 1)[1]]
        elif token.startswith("-Xlambdas="):
            out += ["--x_lambdas", token.split("=", 1)[1]]
        elif token.startswith("-Xsam-conversions="):
            out += ["--x_sam_conversions", token.split("=", 1)[1]]
        elif token.startswith("-XXLanguage:"):
            # The sign is part of the value: `+Feature`, not `Feature`.
            out += ["--x_xlanguage", token.split(":", 1)[1]]
        elif token == "-Xcontext-parameters":
            out += ["--x_context_parameters"]
        elif token == "-Xconsistent-data-class-copy-visibility":
            out += ["--x_consistent_data_class_copy_visibility"]
        elif token.startswith("-Xexplicit-api="):
            out += ["--x_explicit_api", token.split("=", 1)[1]]
        else:
            out += ["--kotlinc-arg", token]
        index += 1
    return out


def scan(model: dict) -> dict:
    """Source counts per module. Java is counted because its presence disqualifies a closure."""
    modules = {}
    for module in model["modules"]:
        kotlin, java = [], 0
        for root in module.get("source_roots", []):
            directory = Path(root["path"])
            if not directory.is_dir():
                continue
            for path in directory.rglob("*"):
                if path.suffix == ".kt":
                    kotlin.append(str(path))
                elif path.suffix == ".java":
                    java += 1
        modules[module["id"]] = {
            "name": module["name"],
            "files": kotlin,
            "java": java,
            "deps": module.get("depends_on", []),
            "cp": module.get("classpath", []),
            "args": module.get("kotlinc_args", []),
            "jvm_target": module.get("jvm_target"),
            # A module with associates is expressed as `--friends` by the rule, which the worker
            # refuses outright (krusty cannot grant cross-module `internal` visibility). Compiling it
            # anyway would manufacture "unresolved reference" on every `internal` reference and hide
            # a refusal we already know about.
            "friends": module.get("friend_paths", []),
        }
    return modules


def closure(modules: dict, start: str) -> set[str]:
    seen, stack = set(), [start]
    while stack:
        current = stack.pop()
        if current in seen or current not in modules:
            continue
        seen.add(current)
        stack.extend(modules[current]["deps"])
    return seen


def buildable(modules: dict, max_files: int) -> list[str]:
    """Modules krusty could build from source alone: no Java anywhere in the closure."""
    found = []
    for module_id, module in modules.items():
        if not module["files"] or module["java"]:
            continue
        reachable = closure(modules, module_id)
        if any(modules[c]["java"] for c in reachable if c in modules):
            continue
        if sum(len(modules[c]["files"]) for c in reachable if c in modules) <= max_files:
            found.append(module_id)
    return found


def topological(modules: dict, targets: list[str]) -> list[str]:
    order, seen = [], set()

    def visit(module_id: str) -> None:
        if module_id in seen or module_id not in modules:
            return
        seen.add(module_id)
        for dependency in modules[module_id]["deps"]:
            visit(dependency)
        order.append(module_id)

    for target in targets:
        visit(target)
    return order


def cause(message: str) -> str:
    lines = [line for line in message.splitlines() if line.strip() and not line.startswith(INERT_PREFIX)]
    if not lines:
        return "(no diagnostic)"
    first = lines[0]
    # Order matters: the CLI's "not modeled by krusty" arrives WRAPPED in the worker's
    # `unsupported by krusty:` prefix, so the broader pattern must be tested last or it swallows it.
    for pattern in ("internal compiler panic", "has no Java front end", "not modeled by krusty",
                    "malformed work request", "unsupported by krusty"):
        if pattern in first:
            return pattern
    return "unresolved reference" if "unresolved reference" in first else "other diagnostic"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", default=DEFAULT_ROOT, help="project checkout to probe")
    parser.add_argument("--krusty", default="target/release/krusty", help="krusty binary")
    parser.add_argument("--krusty-lsp", default="target/release/krusty-lsp", help="krusty-lsp binary")
    parser.add_argument("--limit", type=int, default=0, help="probe at most this many modules")
    parser.add_argument("--max-files", type=int, default=400, help="largest closure to attempt, in .kt files")
    parser.add_argument("--out", default="", help="directory for produced jars (default: a temp dir)")
    parser.add_argument("--json", default="", help="write per-module results here")
    args = parser.parse_args()

    home = java_home()
    if not home:
        # Without a JDK krusty falls back to .kotlin_builtins and invents unresolved references that
        # have nothing to do with the worker; the numbers would be meaningless.
        sys.exit("set JAVA_HOME to a real JDK (needs lib/modules); a JDK-less probe reports fake failures")

    model_json = subprocess.run(
        [args.krusty_lsp, "model", args.root], capture_output=True, text=True, check=True
    ).stdout
    modules = scan(json.loads(model_json))
    targets = buildable(modules, args.max_files)
    targets.sort(key=lambda module_id: len(closure(modules, module_id)))
    if args.limit:
        targets = targets[: args.limit]
    order = topological(modules, targets)
    # `order` also carries the targets' dependencies, so it is larger than the target list; the ones
    # without sources of their own are skipped below and never become requests.
    with_sources = sum(1 for module_id in order if modules[module_id]["files"])
    print(
        f"{len(modules)} modules in the model; {len(targets)} with a Kotlin-only closure; "
        f"{len(order)} in dependency order, {with_sources} with sources to compile"
    )

    jars = Path(args.out) if args.out else Path(os.environ.get("TMPDIR", "/tmp")) / "krusty-bazel-probe"
    # A jar left by an earlier run must not be mistaken for something this run produced.
    if jars.exists():
        for stale in jars.glob("*.jar"):
            stale.unlink()
    jars.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, JAVA_HOME=home)
    # The worker's own fatals and any Rust panic message go to stderr; without capturing it a
    # worker-level failure would be invisible in the results.
    worker_log = Path(args.json).with_suffix(".stderr") if args.json else jars / "worker.stderr"
    stderr_sink = worker_log.open("w")
    worker = subprocess.Popen(
        [args.krusty, "--persistent_worker"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=stderr_sink, env=env, text=True, bufsize=1,
    )

    built: dict[str, Path] = {}
    rows, started = [], time.time()
    aborted = ""
    for request_id, module_id in enumerate(order):
        module = modules[module_id]
        if not module["files"]:
            continue

        # Anything below is measured against an INCOMPLETE classpath unless every Kotlin dependency
        # produced a jar. Compiling regardless turns one upstream failure into a cascade of
        # "unresolved reference" rows that say nothing about the module they are reported against.
        needed = [c for c in closure(modules, module_id) if c != module_id and modules[c]["files"]]
        missing = [c for c in needed if c not in built]
        if missing:
            rows.append({"id": module_id, "name": module["name"], "files": len(module["files"]),
                         "exit": None, "jar": False, "status": "blocked upstream",
                         "output": f"{len(missing)} of {len(needed)} Kotlin dependencies were not built"})
            continue
        if module["friends"]:
            rows.append({"id": module_id, "name": module["name"], "files": len(module["files"]),
                         "exit": None, "jar": False, "status": "refused by the rule (--friends)",
                         "output": f"{len(module['friends'])} associate(s); the worker refuses --friends"})
            continue

        dependency_jars = [str(built[c]) for c in needed]
        jar = jars / f"{module_id.replace(':', '_').replace('/', '_')}.jar"
        # The model names a test module `foo:test`; a bazel label takes one colon, so the target part
        # keeps the suffix instead of producing `//foo:test:lib`.
        label = module["name"].replace(":", "_")
        request = ["--out", str(jar), "--kotlin_module_name", module["name"],
                   "--target_label", f"//{label}:lib"]
        if module["jvm_target"]:
            request += ["--jvm_target", str(module["jvm_target"])]
        request += worker_args(module["args"])
        classpath = dependency_jars + module["cp"]
        if classpath:
            request += ["--cp"] + classpath
        request += ["--srcs"] + module["files"]

        # A worker that dies mid-run must not cost the rows already collected, so every failure here
        # breaks out of the loop and the results are still written below.
        try:
            worker.stdin.write(json.dumps({"arguments": request, "inputs": [], "requestId": request_id}) + "\n")
            worker.stdin.flush()
        except (BrokenPipeError, ValueError):
            aborted = f"worker stdin closed before {module['name']}; it died mid-run"
            break
        line = worker.stdout.readline()
        if not line:
            aborted = f"worker produced no response for {module['name']}; see {worker_log}"
            break
        try:
            response = json.loads(line)
        except json.JSONDecodeError:
            aborted = f"worker wrote a non-JSON line for {module['name']}: {line[:200]!r}"
            break
        ok = response.get("exitCode") == 0 and jar.exists()
        if ok:
            built[module_id] = jar
        rows.append({"id": module_id, "name": module["name"], "files": len(module["files"]),
                     "exit": response.get("exitCode"), "jar": jar.exists(),
                     "status": "built" if ok else "failed",
                     "output": response.get("output", "")[:2000]})

    try:
        worker.stdin.close()
        worker.wait(timeout=120)
    except (subprocess.TimeoutExpired, BrokenPipeError, ValueError):
        worker.kill()
        worker.wait()
    stderr_sink.close()

    if args.json:
        Path(args.json).write_text(json.dumps(rows, indent=1))
    if aborted:
        print(f"ABORTED: {aborted}", file=sys.stderr)
    ok_rows = [row for row in rows if row["status"] == "built"]
    attempted = [row for row in rows if row["status"] in ("built", "failed")]
    # Rows never sent to the worker are reported separately: counting them as krusty failures would
    # attribute an upstream break, or a refusal the rule makes, to the module they appear against.
    skipped = Counter(row["status"] for row in rows if row["status"] not in ("built", "failed"))
    causes = Counter(cause(row["output"]) for row in attempted if row["status"] == "failed")
    print(f"\none worker process, {len(attempted)} requests, {time.time() - started:.1f}s, shutdown={worker.returncode}")
    print(f"built {len(ok_rows)}/{len(attempted)} attempted ({sum(row['files'] for row in ok_rows)} .kt files)")
    for reason, count in causes.most_common():
        print(f"  {count:4d}  {reason}")
    if skipped:
        print(f"not attempted ({sum(skipped.values())} of {len(rows)} modules with sources):")
        for reason, count in skipped.most_common():
            print(f"  {count:4d}  {reason}")


if __name__ == "__main__":
    main()
