#!/usr/bin/env python3
"""Whole-project parity scan: run krusty over every module of a real worktree and cluster the result.

The scan itself is `krusty-lsp parity`, which resolves the project model, plans one analysis per
module, and runs each module in its own child process (so a module that hangs or dies costs one
module, not the run). This script drives that binary and turns its JSONL into the two things a
person actually reads: how many modules come back clean, and which handful of root causes account
for everything that does not.

    scripts/parity-scan.py ~/external-projects/intellij-community
    scripts/parity-scan.py <root> --jsonl out.jsonl --markdown docs/PROJECT_PARITY.md

Re-aggregating an existing run costs nothing — pass --from to skip the scan:

    scripts/parity-scan.py <root> --from out.jsonl
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
import re
import shlex
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Message normalization: a cluster is a root cause, not a wording. Identifiers, literals and numbers
# are what vary between two reports of the SAME gap, so they are what gets folded away.
NORMALIZERS = [
    (re.compile(r"'[^']*'"), "'_'"),
    (re.compile(r'"[^"]*"'), '"_"'),
    (re.compile(r"`[^`]*`"), "`_`"),
    (re.compile(r"\b\d+\b"), "N"),
]


def normalize(message: str) -> str:
    text = message.strip()
    for pattern, replacement in NORMALIZERS:
        text = pattern.sub(replacement, text)
    return text


# A diagnostic naming an identifier the project itself declares, in a scan that could not see that
# declaration (the dependency module has no built output), is an artifact of the scan — not a krusty
# gap. These are the message shapes that carry such a name.
NAMED_UNRESOLVED = [
    re.compile(r"unresolved reference '([^']+)'"),
    re.compile(r"unresolved function '([^']+)'"),
    re.compile(r"supertype '([^']+)' could not be resolved"),
    re.compile(r"unresolved super method '([^']+)'"),
    re.compile(r"unresolved Java static '([^']+)'"),
]

# Mirrors PARITY_SKIP_DIRECTORIES in crates/krusty-lsp/src/main.rs. The Rust list decides which
# files are compiled; this one decides which names are excused, so they must agree — a name excused
# here that the scan never had a chance to see is exactly the case this filter exists for.
SKIP_DIRECTORIES = {
    ".git",
    "testData",
    "test-data",
    "testdata",
    "out",
    "build",
    "target",
    "node_modules",
}


def project_declared_names(model: dict | None, include_tests: bool) -> set[str]:
    """Type names the project declares, approximated by source file stem.

    Deliberately not a parse: Java requires the public type to match the file name and Kotlin follows
    the same convention for most declarations, so the stem is a good approximation at a directory
    walk's cost rather than 80k parses.

    Restricted to the model's own source roots when a model is available. Walking the whole worktree
    instead sweeps in test fixtures and generated trees, and every extra name widens the set of
    errors this filter excuses.
    """
    # A whole-tree fallback sweeps in test fixtures and generated trees whose names were never
    # eligible inputs. That can excuse a real production diagnostic. If the model is unavailable,
    # disable the heuristic instead; under-counting scan artifacts is conservative.
    if not model:
        return set()
    roots = [
        Path(entry["path"])
        for module in model.get("modules", [])
        for entry in module.get("source_roots", [])
        if include_tests or entry.get("kind") != "test"
    ]
    names: set[str] = set()
    for source_root in roots:
        for _, directories, filenames in os.walk(source_root):
            directories[:] = [name for name in directories if name not in SKIP_DIRECTORIES]
            for filename in filenames:
                stem, dot, extension = filename.rpartition(".")
                if dot and extension in ("kt", "java"):
                    names.add(stem)
    return names


def resolve_model(binary: Path | None, root: Path) -> dict | None:
    if binary is None or not binary.exists():
        return None
    try:
        finished = subprocess.run(
            [str(binary), "model", str(root)], capture_output=True, text=True, timeout=300
        )
        return json.loads(finished.stdout) if finished.returncode == 0 else None
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def unseen_declaration(message: str, declared: set[str], visible: set[str] | None) -> bool:
    """Does this error name a declaration the scan could not have seen?

    The name must look like a type (a leading capital): `unresolved reference` is also emitted for
    members and locals, and matching those against the file-stem set would excuse real gaps whose
    identifier happens to coincide with some file name. Even so this is a HEURISTIC — the report
    states its total separately instead of folding it into a pass rate.
    """
    # Old reports did not record the declarations that were actually visible to a module. Without
    # that evidence the heuristic could excuse a real resolver failure in an input the compiler did
    # see, so be conservative and classify nothing as an unbuilt-module artifact.
    if visible is None:
        return False
    for pattern in NAMED_UNRESOLVED:
        match = pattern.search(message)
        if not match:
            continue
        name = match.group(1).split(".")[-1]
        if name[:1].isupper() and name in declared and name not in visible:
            return True
    return False


def locate_binary(explicit: str | None) -> Path | None:
    """The krusty-lsp to drive, or None when there is none built."""
    if explicit:
        return Path(explicit)
    candidates = [
        REPO / "target" / profile / "krusty-lsp"
        for profile in ("release", "gate", "debug")
    ]
    candidates = [candidate for candidate in candidates if candidate.is_file()]
    return max(candidates, key=lambda candidate: candidate.stat().st_mtime_ns, default=None)


def find_binary(explicit: str | None) -> Path:
    """The binary, or exit. Only for the SCAN — re-aggregation must not require a built compiler."""
    binary = locate_binary(explicit)
    if binary is None:
        sys.exit(
            "krusty-lsp binary not found; build it with `cargo build -p krusty-lsp --release` "
            "or pass --binary"
        )
    return binary


def git_revision(root: Path) -> str | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        return result.stdout.strip() if result.returncode == 0 else None
    except (OSError, subprocess.SubprocessError):
        return None


def git_dirty(root: Path) -> bool | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "status", "--porcelain"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        return bool(result.stdout) if result.returncode == 0 else None
    except (OSError, subprocess.SubprocessError):
        return None


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def metadata_path(jsonl: Path) -> Path:
    return Path(f"{jsonl}.meta.json")


def write_metadata(jsonl: Path, metadata: dict) -> None:
    path = metadata_path(jsonl)
    try:
        path.write_text(json.dumps(metadata, indent=2) + "\n")
    except OSError as error:
        sys.exit(f"cannot write scan metadata {path}: {error}")


def run_scan(binary: Path, root: Path, jsonl: Path, args: argparse.Namespace) -> dict:
    model = resolve_model(binary, root)
    declared_names = project_declared_names(model, args.tests)
    project_revision = git_revision(root)
    project_dirty = git_dirty(root)
    harness_revision = git_revision(REPO)
    harness_dirty = git_dirty(REPO)
    binary_hash = file_sha256(binary)
    metadata = {
        "schema": 1,
        # Written before the child starts. An interrupt or failed scan therefore leaves an
        # explicitly incomplete sidecar instead of a plausible-looking partial JSONL.
        "complete": False,
        "root": str(root),
        "project_revision": project_revision,
        "project_worktree_dirty": project_dirty,
        "harness_repository_revision": harness_revision,
        "harness_worktree_dirty": harness_dirty,
        "harness_script_sha256": file_sha256(Path(__file__)),
        "compiler_binary": str(binary.resolve()),
        "compiler_binary_sha256": binary_hash,
        # Preserve the exact set used by the heuristic. Re-aggregation must not silently change
        # classification because the worktree or the model-producing binary changed afterward.
        "declared_names": sorted(declared_names),
        "depth": args.depth,
        "tests": args.tests,
        "jobs": args.jobs,
        "timeout_seconds": args.timeout,
        "filter": args.filter,
        "limit": args.limit,
    }
    write_metadata(jsonl, metadata)
    command = [
        str(binary),
        "parity",
        str(root),
        "--out",
        str(jsonl),
        "--jobs",
        str(args.jobs),
        "--timeout",
        str(args.timeout),
        "--depth",
        args.depth,
    ]
    if args.tests:
        command.append("--tests")
    if args.filter:
        command += ["--filter", args.filter]
    if args.limit:
        command += ["--limit", str(args.limit)]
    started = time.time()
    result = subprocess.run(command)
    try:
        final_binary_hash = file_sha256(binary)
    except OSError:
        final_binary_hash = None
    metadata["final_project_revision"] = git_revision(root)
    metadata["final_project_worktree_dirty"] = git_dirty(root)
    metadata["final_harness_repository_revision"] = git_revision(REPO)
    metadata["final_harness_worktree_dirty"] = git_dirty(REPO)
    metadata["final_compiler_binary_sha256"] = final_binary_hash
    changed = any(
        (
            metadata["project_revision"] != metadata["final_project_revision"],
            metadata["project_worktree_dirty"] != metadata["final_project_worktree_dirty"],
            metadata["harness_repository_revision"]
            != metadata["final_harness_repository_revision"],
            metadata["harness_worktree_dirty"] != metadata["final_harness_worktree_dirty"],
            metadata["compiler_binary_sha256"] != metadata["final_compiler_binary_sha256"],
        )
    )
    metadata["inputs_changed_during_scan"] = changed
    metadata["complete"] = result.returncode == 0 and not changed
    write_metadata(jsonl, metadata)
    if result.returncode != 0:
        sys.exit(f"parity scan failed with exit code {result.returncode}")
    if changed:
        sys.exit("project, harness, or compiler binary changed during the scan; raw output retained")
    print(f"scan finished in {time.time() - started:.1f}s -> {jsonl}", file=sys.stderr)
    return metadata


def load_metadata(jsonl: Path) -> dict:
    path = metadata_path(jsonl)
    if not path.is_file():
        return {}
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError) as error:
        sys.exit(f"cannot read scan metadata {path}: {error}")


def load(jsonl: Path) -> list[dict]:
    reports = []
    with jsonl.open() as handle:
        for line in handle:
            line = line.strip()
            if line:
                reports.append(json.loads(line))
    return reports


def summarize(
    reports: list[dict], top: int, declared: set[str] | None = None, metadata: dict | None = None
) -> dict:
    declared = declared or set()
    status = collections.Counter(report["status"] for report in reports)
    clusters: collections.Counter[str] = collections.Counter()
    examples: dict[str, dict] = {}
    modules_per_cluster: dict[str, set[str]] = collections.defaultdict(set)
    unseen = 0
    modules_clean_but_for_unseen = 0
    for report in reports:
        recorded_visible = report.get("visible_declarations")
        visible = set(recorded_visible) if recorded_visible is not None else None
        real_errors = 0
        for diagnostic in report.get("diagnostics", []):
            if unseen_declaration(diagnostic["message"], declared, visible):
                unseen += 1
                continue
            real_errors += 1
            key = normalize(diagnostic["message"])
            clusters[key] += 1
            modules_per_cluster[key].add(report["module"])
            examples.setdefault(
                key,
                {
                    "message": diagnostic["message"],
                    "file": diagnostic["file"],
                    "line": diagnostic["line"],
                    "module": report["module"],
                },
            )
        if report["status"] != "ok" and real_errors == 0 and report["status"].startswith("errors"):
            modules_clean_but_for_unseen += 1
    checked = sum(report.get("checked_files", 0) for report in reports)
    clean = status.get("ok", 0)
    return {
        "run": metadata or {},
        "truncated_modules": sum(1 for report in reports if report.get("truncated")),
        "unreadable_checked_files": sum(
            report.get("unreadable_checked_files", 0) for report in reports
        ),
        "java_stub_failed_modules": sum(
            1 for report in reports if report.get("java_stub_failed")
        ),
        "modules": len(reports),
        "clean_modules": clean,
        "clean_but_for_unseen": modules_clean_but_for_unseen,
        "checked_files": checked,
        "status": dict(status),
        "unseen_declaration_errors": unseen,
        "total_errors": sum(report.get("error_count", 0) for report in reports),
        "slowest": sorted(
            ((report["elapsed_ms"], report["module"]) for report in reports), reverse=True
        )[:top],
        "worst_modules": sorted(
            (
                (report.get("error_count", 0), report["module"])
                for report in reports
                if report.get("error_count", 0)
            ),
            reverse=True,
        )[:top],
        "clusters": [
            {
                "count": count,
                "modules": len(modules_per_cluster[key]),
                "pattern": key,
                "example": examples[key],
            }
            for key, count in clusters.most_common(top)
        ],
        "cluster_total": len(clusters),
    }


def render_text(summary: dict) -> str:
    lines = []
    modules = summary["modules"]
    clean = summary["clean_modules"]
    share = (clean / modules * 100) if modules else 0.0
    lines.append(f"modules      {clean}/{modules} clean ({share:.1f}%)")
    lines.append(f"kotlin files {summary['checked_files']}")
    lines.append(f"errors       {summary['total_errors']} total")
    lines.append(
        f"  of which   {summary['unseen_declaration_errors']} name a declaration the scan could "
        f"not see (unbuilt dependency module)"
    )
    lines.append(
        f"  remaining  {summary['total_errors'] - summary['unseen_declaration_errors']} in "
        f"{summary['cluster_total']} clusters"
    )
    lines.append(
        f"modules whose ONLY errors are unseen declarations: {summary['clean_but_for_unseen']}"
    )
    lines.append(
        f"caveats      {summary['truncated_modules']} module(s) hit the input budget, "
        f"{summary['unreadable_checked_files']} checked file(s) unreadable, "
        f"{summary['java_stub_failed_modules']} Java stub overlay(s) failed"
    )
    run = summary.get("run", {})
    if run:
        project_dirty = "+dirty" if run.get("project_worktree_dirty") else ""
        harness_dirty = "+dirty" if run.get("harness_worktree_dirty") else ""
        lines.append(
            "run          "
            f"project={run.get('project_revision') or 'unknown'}{project_dirty} "
            f"harness={run.get('harness_repository_revision') or 'unknown'}{harness_dirty} "
            f"binary-sha256={run.get('compiler_binary_sha256') or 'unknown'} "
            f"depth={run.get('depth', 'unknown')} tests={run.get('tests', 'unknown')}"
        )
    lines.append(f"status       {summary['status']}")
    lines.append("")
    lines.append("top error clusters:")
    for cluster in summary["clusters"]:
        example = cluster["example"]
        lines.append(
            f"  {cluster['count']:6d}  {cluster['modules']:4d} mod  {cluster['pattern'][:100]}"
        )
        lines.append(f"          e.g. {example['file']}:{example['line']}")
    lines.append("")
    lines.append("slowest modules (ms):")
    for elapsed, module in summary["slowest"]:
        lines.append(f"  {elapsed:8d}  {module}")
    return "\n".join(lines)


def render_markdown(summary: dict, root: Path, jsonl: Path) -> str:
    modules = summary["modules"]
    clean = summary["clean_modules"]
    share = (clean / modules * 100) if modules else 0.0
    run = summary.get("run", {})
    invocation = ["scripts/parity-scan.py", "<project-root>"]
    if run.get("depth") is not None:
        invocation += ["--depth", str(run["depth"])]
    if run.get("tests"):
        invocation.append("--tests")
    if run.get("jobs") is not None:
        invocation += ["--jobs", str(run["jobs"])]
    if run.get("timeout_seconds") is not None:
        invocation += ["--timeout", str(run["timeout_seconds"])]
    if run.get("filter"):
        invocation += ["--filter", str(run["filter"])]
    if run.get("limit"):
        invocation += ["--limit", str(run["limit"])]
    project_dirty = " (dirty worktree)" if run.get("project_worktree_dirty") else ""
    harness_dirty = " (dirty worktree)" if run.get("harness_worktree_dirty") else ""
    lines = [
        "# Project parity",
        "",
        f"How much of `{root.name}` krusty's front end accepts today. Regenerate with:",
        "",
        "```bash",
        shlex.join(invocation),
        "```",
        "",
        f"Project revision: `{run.get('project_revision') or 'unknown'}`{project_dirty}  ",
        f"Compiler binary SHA-256: `{run.get('compiler_binary_sha256') or 'unknown'}`  ",
        f"Harness repository revision: `{run.get('harness_repository_revision') or 'unknown'}`{harness_dirty}",
        "",
        "## How to read this",
        "",
        "Each module is analyzed the way the language server would: its own Kotlin sources checked,",
        "its Java sources supplied as stubs, and its declared jar classpath plus the platform JDK.",
        "The scanned worktree has no built module outputs, so a reference into another module of the",
        "same project cannot resolve. Those errors are counted separately (they name a type the",
        "project itself declares by file stem, and that stem was absent from the module's recorded",
        "checked/inferred/Java inputs). They are NOT part of the clusters below. This is a",
        "conservative heuristic, not an adjusted module pass rate.",
        "",
        "## Headline",
        "",
        "| measure | value |",
        "| --- | --- |",
        f"| modules scanned | {modules} |",
        f"| modules with zero errors | {clean} ({share:.1f}%) |",
        f"| modules whose only errors name an unbuilt dependency | {summary['clean_but_for_unseen']} |",
        f"| Kotlin files checked | {summary['checked_files']} |",
        f"| error diagnostics | {summary['total_errors']} |",
        f"| … naming a declaration the scan could not see | {summary['unseen_declaration_errors']} |",
        f"| … remaining, clustered below | {summary['total_errors'] - summary['unseen_declaration_errors']} |",
        f"| distinct error clusters | {summary['cluster_total']} |",
        f"| modules that hit the input budget | {summary['truncated_modules']} |",
        f"| checked files that could not be read | {summary['unreadable_checked_files']} |",
        f"| modules whose Java stub overlay failed | {summary['java_stub_failed_modules']} |",
        "",
        "## Module outcomes",
        "",
        "A module that timed out or crashed reports zero files and zero errors, so it counts in the",
        "denominator above without contributing to any cluster.",
        "",
        "| status | modules |",
        "| --- | ---: |",
    ]
    for name, count in sorted(summary["status"].items(), key=lambda entry: -entry[1]):
        lines.append(f"| `{name}` | {count} |")
    lines += [
        "",
        "## Top error clusters",
        "",
        "| errors | modules | pattern | example |",
        "| ---: | ---: | --- | --- |",
    ]
    for cluster in summary["clusters"]:
        example = cluster["example"]
        pattern = cluster["pattern"].replace("|", "\\|")
        location = f"{Path(example['file']).name}:{example['line']}"
        lines.append(
            f"| {cluster['count']} | {cluster['modules']} | {pattern} | `{location}` |"
        )
    lines += ["", "## Slowest modules", "", "| ms | module |", "| ---: | --- |"]
    for elapsed, module in summary["slowest"]:
        lines.append(f"| {elapsed} | {module} |")
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", help="project worktree to scan")
    parser.add_argument("--binary", help="krusty-lsp binary (default: newest built)")
    parser.add_argument("--jsonl", help="where to write per-module records")
    parser.add_argument("--from", dest="reuse", help="aggregate an existing JSONL, no scan")
    parser.add_argument("--markdown", help="also write a markdown report to this path")
    parser.add_argument("--json", dest="json_out", help="also write the summary as JSON")
    # One worker by default, matching `krusty-lsp parity`: each worker is a full compiler process,
    # and this binary is also the editor's language server. Fanning out is an explicit choice.
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--timeout", type=int, default=120, help="per-module seconds")
    parser.add_argument("--depth", default="direct", choices=["none", "direct", "all"])
    parser.add_argument("--tests", action="store_true", help="include test source roots")
    parser.add_argument("--filter", default="", help="module id substring")
    parser.add_argument("--limit", type=int, default=0, help="scan at most N modules")
    parser.add_argument("--top", type=int, default=25)
    parser.add_argument(
        "--no-declaration-filter",
        action="store_true",
        help="count every error, including ones naming a project declaration the scan could not see",
    )
    args = parser.parse_args()

    root = Path(args.root).expanduser().resolve()
    if args.reuse:
        jsonl = Path(args.reuse)
        metadata = load_metadata(jsonl)
        if metadata.get("complete") is False:
            sys.exit(f"refusing to aggregate incomplete scan {jsonl}")
    else:
        jsonl = Path(args.jsonl) if args.jsonl else REPO / "target" / "parity" / f"{root.name}.jsonl"
        jsonl.parent.mkdir(parents=True, exist_ok=True)
        binary = find_binary(args.binary)
        metadata = run_scan(binary, root, jsonl, args)

    reports = load(jsonl)
    declared = (
        set()
        if args.no_declaration_filter
        # The sidecar freezes the exact declaration set from scan time. Old or incomplete sidecars
        # disable the heuristic rather than recomputing it against a potentially different tree.
        else set(metadata.get("declared_names", []))
    )
    summary = summarize(reports, args.top, declared, metadata)
    print(render_text(summary))
    if args.markdown:
        Path(args.markdown).write_text(render_markdown(summary, root, jsonl))
        print(f"\nwrote {args.markdown}", file=sys.stderr)
    if args.json_out:
        Path(args.json_out).write_text(json.dumps(summary, indent=2) + "\n")


if __name__ == "__main__":
    main()
