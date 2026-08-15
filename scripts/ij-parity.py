#!/usr/bin/env python3
"""intellij-community parity run, with the settings that repository actually needs.

A thin, opinionated wrapper over `scripts/parity-scan.py`:

  * points at the intellij-community checkout (override with --root or KRUSTY_IJ_ROOT);
  * refuses to run without a JDK, because a JDK-less scan reports every `java.*` reference as
    unresolved and the resulting numbers look like krusty gaps when they are the harness's fault;
  * writes both the raw JSONL and the markdown baseline the repository tracks.

    scripts/ij-parity.py                    # scan, print summary, refresh docs/PROJECT_PARITY.md
    scripts/ij-parity.py --from run.jsonl   # re-aggregate an earlier run
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEFAULT_ROOT = Path("~/external-projects/intellij-community").expanduser()


def has_modules(home: str) -> bool:
    """A JDK krusty can actually read: `lib/modules` is the jimage the classpath decodes."""
    return (Path(home) / "lib" / "modules").is_file()


def java_home() -> str | None:
    # An already-set JAVA_HOME is checked like any other candidate. Trusting it defeats the whole
    # point of this guard: a stale or JRE-only path passes, and the scan silently measures the
    # `.kotlin_builtins` fallback instead of the project.
    configured = os.environ.get("JAVA_HOME")
    if configured and has_modules(configured):
        return configured
    if configured:
        print(
            f"ij-parity: ignoring JAVA_HOME={configured} (no lib/modules)",
            file=sys.stderr,
        )
    for candidate in (
        "/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home",
        "/usr/lib/jvm/java-21-openjdk",
    ):
        if has_modules(candidate):
            return candidate
    return None


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=os.environ.get("KRUSTY_IJ_ROOT", str(DEFAULT_ROOT)))
    parser.add_argument("--from", dest="reuse", help="aggregate an existing JSONL, no scan")
    parser.add_argument("--depth", default="none", choices=["none", "direct", "all"])
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--tests", action="store_true")
    parser.add_argument("--filter", default="")
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--top", type=int, default=25)
    parser.add_argument(
        "--no-declaration-filter",
        action="store_true",
        help="count every error, including ones naming a project declaration the scan could not see",
    )
    parser.add_argument(
        "--no-markdown", action="store_true", help="do not refresh docs/PROJECT_PARITY.md"
    )
    args = parser.parse_args()

    root = Path(args.root).expanduser()
    if not (root / ".idea" / "modules.xml").is_file():
        sys.exit(f"{root} does not look like an intellij-community checkout (.idea/modules.xml)")

    environment = dict(os.environ)
    if not args.reuse:
        home = java_home()
        if not home:
            sys.exit(
                "no JDK found: set JAVA_HOME to a JDK with lib/modules. Scanning without one "
                "reports java.* as unresolved and the numbers would be meaningless."
            )
        environment["JAVA_HOME"] = home

    jsonl = REPO / "target" / "parity" / "intellij-community.jsonl"
    jsonl.parent.mkdir(parents=True, exist_ok=True)
    command = [
        sys.executable,
        str(REPO / "scripts" / "parity-scan.py"),
        str(root),
        "--jsonl",
        str(jsonl),
        "--depth",
        args.depth,
        "--jobs",
        str(args.jobs),
        "--timeout",
        str(args.timeout),
    ]
    if args.reuse:
        command += ["--from", args.reuse]
    if args.tests:
        command.append("--tests")
    if args.filter:
        command += ["--filter", args.filter]
    if args.limit:
        command += ["--limit", str(args.limit)]
    command += ["--top", str(args.top)]
    if args.no_declaration_filter:
        command.append("--no-declaration-filter")
    if not args.no_markdown:
        command += ["--markdown", str(REPO / "docs" / "PROJECT_PARITY.md")]
    raise SystemExit(subprocess.run(command, env=environment).returncode)


if __name__ == "__main__":
    main()
