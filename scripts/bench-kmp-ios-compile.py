#!/usr/bin/env python3
"""Time krusty, kotlinc, and swiftc on the kmp-ios-benchmark generated sources.

The articles time an iOS app build (xcodebuild, and for Kotlin a Gradle framework
link before that). This script times the compilers themselves on the sources that
harness generates: a clean compile of N files, then a rebuild of that same tree
after one generated file changes. Each figure is the median of three trials after
one warmup.

krusty and kotlinc perform a full rebuild after the edit. swiftc is asked for
`-incremental` on the second invocation, with an output file map, so it can rebuild
only the edited file. The report labels these different compilation models explicitly.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path


def load_generator(path: Path):
    spec = importlib.util.spec_from_file_location("kmp_ios_generate", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot load generator: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run(cmd: list[str], env: dict[str, str]) -> tuple[int, float, str]:
    started = time.perf_counter()
    completed = subprocess.run(cmd, env=env, capture_output=True, text=True)
    elapsed = time.perf_counter() - started
    return completed.returncode, elapsed, (completed.stdout + completed.stderr)[-1200:]


def median(samples: list[float]) -> float:
    return statistics.median(samples)


class Bench:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.env = os.environ.copy()
        # The selected compiler distribution owns both stdlib and reflect. Derive krusty's
        # distribution from the exact kotlinc executable being benchmarked instead of relying on
        # an unrelated ambient installation.
        self.env["KRUSTY_KOTLINC"] = str(Path(args.kotlinc).resolve())
        self.gen = load_generator(Path(args.generator))
        self.root = Path(args.work)
        if self.root.exists():
            shutil.rmtree(self.root)
        self.root.mkdir(parents=True)
        self.rows: list[dict[str, object]] = []

    def kotlin_sources(self, count: int) -> Path:
        dest = self.root / f"kotlin-{count}"
        if dest.exists():
            shutil.rmtree(dest)
        dest.mkdir(parents=True)
        self.gen.generate_kotlin_files(dest, count)
        return dest

    def swift_sources(self, count: int) -> Path:
        dest = self.root / f"swift-{count}"
        if dest.exists():
            shutil.rmtree(dest)
        dest.mkdir(parents=True)
        self.gen.generate_swift_files(dest, count)
        return dest

    def edit_kotlin(self, src: Path) -> None:
        path = src / "Generated0001.kt"
        text = path.read_text()
        old = "val value: Int = 1,"
        if old not in text:
            raise SystemExit(f"toggle line missing in {path}")
        path.write_text(text.replace(old, "val value: Int = 1001,", 1))

    def edit_swift(self, src: Path) -> None:
        path = src / "Generated0001.swift"
        text = path.read_text()
        old = "value: Int = 1)"
        if old not in text:
            raise SystemExit(f"toggle line missing in {path}")
        path.write_text(text.replace(old, "value: Int = 1001)", 1))

    def compile_krusty(self, src: Path, out: Path) -> tuple[int, float, str, int]:
        if out.exists():
            shutil.rmtree(out)
        out.mkdir(parents=True)
        code, elapsed, tail = run(
            [self.args.krusty, "-cp", self.args.stdlib, "-d", str(out), str(src)],
            self.env,
        )
        classes = sum(1 for _ in out.rglob("*.class"))
        return code, elapsed, tail, classes

    def compile_kotlinc(self, src: Path, out: Path) -> tuple[int, float, str, int]:
        if out.exists():
            shutil.rmtree(out)
        out.mkdir(parents=True)
        code, elapsed, tail = run(
            [self.args.kotlinc, "-cp", self.args.stdlib, "-d", str(out), str(src)],
            self.env,
        )
        classes = sum(1 for _ in out.rglob("*.class"))
        return code, elapsed, tail, classes

    def swift_output_map(self, src: Path, out: Path) -> Path:
        mapping: dict[str, dict[str, str]] = {
            "": {"swift-dependencies": str(out / "master.swiftdeps")},
        }
        for path in sorted(src.glob("*.swift")):
            mapping[str(path)] = {
                "object": str(out / f"{path.stem}.o"),
                "swift-dependencies": str(out / f"{path.stem}.swiftdeps"),
            }
        map_path = out / "output-file-map.json"
        map_path.write_text(json.dumps(mapping))
        return map_path

    def compile_swift(self, src: Path, out: Path, incremental: bool) -> tuple[int, float, str, int]:
        out.mkdir(parents=True, exist_ok=True)
        cmd = [
            self.args.swiftc,
            "-parse-as-library",
            "-module-name",
            "Bench",
            "-emit-object",
            "-output-file-map",
            str(self.swift_output_map(src, out)),
        ]
        if incremental:
            cmd.append("-incremental")
        cmd.extend(sorted(str(path) for path in src.glob("*.swift")))
        code, elapsed, tail = run(cmd, self.env)
        objects = sum(1 for _ in out.glob("*.o"))
        return code, elapsed, tail, objects

    def kotlin_trials(
        self, count: int, label: str, compile
    ) -> tuple[list[float], list[float], int]:
        clean_times: list[float] = []
        rebuild_times: list[float] = []
        produced = 0
        for index in range(4):
            src = self.kotlin_sources(count)
            out = self.root / f"out-{label}-{count}"
            code, elapsed, tail, produced = compile(src, out)
            kind = "warmup" if index == 0 else f"trial {index}"
            print(
                f"  {label} clean {count} {kind}: {elapsed:.3f}s "
                f"classes={produced} exit={code}",
                flush=True,
            )
            if code != 0:
                print(tail)
                raise SystemExit(f"{label} clean compile failed")
            if index > 0:
                clean_times.append(elapsed)

            self.edit_kotlin(src)
            code, elapsed, tail, rebuilt = compile(src, out)
            print(
                f"  {label} full rebuild after edit {count} {kind}: {elapsed:.3f}s "
                f"classes={rebuilt} exit={code}",
                flush=True,
            )
            if code != 0:
                print(tail)
                raise SystemExit(f"{label} rebuild after edit failed")
            if rebuilt != produced:
                raise SystemExit(
                    f"{label} class count changed after edit at {count}: "
                    f"clean {produced}, rebuilt {rebuilt}"
                )
            if index > 0:
                rebuild_times.append(elapsed)
        return clean_times, rebuild_times, produced

    def swift_trials(self, count: int) -> tuple[list[float], list[float], int]:
        clean_times: list[float] = []
        inc_times: list[float] = []
        objects = 0
        for index in range(4):
            src = self.swift_sources(count)
            out = self.root / f"out-swift-{count}"
            if out.exists():
                shutil.rmtree(out)
            code, elapsed, tail, objects = self.compile_swift(src, out, incremental=False)
            kind = "warmup" if index == 0 else f"trial {index}"
            print(f"  swift clean {count} {kind}: {elapsed:.3f}s objects={objects} exit={code}", flush=True)
            if code != 0:
                print(tail)
                raise SystemExit("swift clean failed")
            if index > 0:
                clean_times.append(elapsed)
            self.edit_swift(src)
            code, elapsed, tail, objects = self.compile_swift(src, out, incremental=True)
            print(
                f"  swift incremental {count} {kind}: {elapsed:.3f}s objects={objects} exit={code}",
                flush=True,
            )
            if code != 0:
                print(tail)
                raise SystemExit("swift incremental failed")
            if index > 0:
                inc_times.append(elapsed)
        return clean_times, inc_times, objects

    def measure(self, counts: list[int]) -> None:
        for count in counts:
            print(f"\n== {count} files ==", flush=True)
            krusty_clean, krusty_rebuild, krusty_classes = self.kotlin_trials(
                count, "krusty", self.compile_krusty
            )
            kotlinc_clean, kotlinc_rebuild, kotlinc_classes = self.kotlin_trials(
                count, "kotlinc", self.compile_kotlinc
            )
            if krusty_classes != kotlinc_classes:
                raise SystemExit(
                    f"class count mismatch at {count}: krusty {krusty_classes}, kotlinc {kotlinc_classes}"
                )
            swift_clean, swift_inc, swift_objects = self.swift_trials(count)
            self.rows.append(
                {
                    "files": count,
                    "krusty_clean_s": median(krusty_clean),
                    "kotlinc_clean_s": median(kotlinc_clean),
                    "swift_clean_s": median(swift_clean),
                    "krusty_rebuild_s": median(krusty_rebuild),
                    "kotlinc_rebuild_s": median(kotlinc_rebuild),
                    "swift_incremental_s": median(swift_inc),
                    "classes": krusty_classes,
                    "swift_objects": swift_objects,
                }
            )

    def report(self) -> str:
        lines = [
            "| Files | krusty clean | kotlinc clean | swiftc clean | krusty full rebuild after edit | kotlinc full rebuild after edit | swiftc incremental after edit |",
            "|------:|-------------:|--------------:|-------------:|-------------------------------:|--------------------------------:|------------------------------:|",
        ]
        for row in self.rows:
            lines.append(
                "| {files} | {krusty_clean_s:.2f}s | {kotlinc_clean_s:.2f}s | {swift_clean_s:.2f}s "
                "| {krusty_rebuild_s:.2f}s | {kotlinc_rebuild_s:.2f}s | {swift_incremental_s:.2f}s |".format(
                    **row
                )
            )
        text = "\n".join(lines)
        print("\n" + text, flush=True)
        return text


def host_lines() -> list[str]:
    lines = [f"host: {platform.platform()}", f"machine: {platform.machine()}", f"cpus: {os.cpu_count()}"]
    if platform.system() == "Darwin":
        for key in ("machdep.cpu.brand_string", "hw.memsize"):
            completed = subprocess.run(["sysctl", "-n", key], capture_output=True, text=True)
            if completed.returncode == 0:
                lines.append(f"{key}: {completed.stdout.strip()}")
    return lines


def version_line(cmd: list[str]) -> str:
    completed = subprocess.run(cmd, capture_output=True, text=True)
    text = (completed.stdout + completed.stderr).strip().splitlines()
    return text[0] if text else f"{cmd[0]} (no version output, exit {completed.returncode})"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--generator", required=True, help="path to kmp-ios-benchmark bench/generate_code.py")
    parser.add_argument("--krusty", required=True)
    parser.add_argument("--kotlinc", required=True)
    parser.add_argument("--stdlib", required=True)
    parser.add_argument("--swiftc", default="swiftc")
    parser.add_argument("--work", default="target/bench-kmp-ios")
    parser.add_argument("--summary", help="write the markdown table here")
    parser.add_argument("counts", nargs="*", type=int, default=[100, 200, 400])
    args = parser.parse_args()

    versions = [
        version_line([args.krusty, "-version"]),
        version_line([args.kotlinc, "-version"]),
        version_line([args.swiftc, "--version"]),
    ]
    for line in host_lines() + versions:
        print(line, flush=True)

    bench = Bench(args)
    bench.measure(args.counts or [100, 200, 400])
    table = bench.report()
    summary = "\n".join(host_lines() + versions + ["", table, ""])
    if args.summary:
        Path(args.summary).write_text(summary)
    step_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary:
        with open(step_summary, "a", encoding="utf-8") as handle:
            handle.write(summary)


if __name__ == "__main__":
    main()
