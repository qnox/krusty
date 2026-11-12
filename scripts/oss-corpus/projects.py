#!/usr/bin/env python3
"""Read scripts/oss-corpus/projects.tsv.

One project per line, tab-separated:

    name, git URL, commit, Gradle root, JDK, Kotlin version pin, [Android variant], [lane]

The lane is recognized only when the last field is `lsp`, `compile`, or `compile,lsp`. Any other
last field is data (a version pin or an Android variant), and the row belongs to both sets.
`compile` is what the corpus builds. `lsp` is what `scripts/parity-scan.py` measures.
"""

from __future__ import annotations

import argparse
import re
from pathlib import Path

LANES = {"lsp", "compile", "compile,lsp"}
COMMIT = re.compile(r"^[0-9a-f]{40}$")
HERE = Path(__file__).resolve().parent
DEFAULT_FILE = HERE / "projects.tsv"


def parse(text: str, path: str = "projects.tsv") -> list[dict]:
    rows = []
    for lineno, line in enumerate(text.splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) < 6 or not fields[0] or not fields[1] or not COMMIT.fullmatch(fields[2]):
            raise ValueError(f"{path}:{lineno}: not a project line")
        lane = fields[-1] if fields[-1] in LANES else "compile,lsp"
        rows.append(
            {"name": fields[0], "url": fields[1], "commit": fields[2], "lane": lane}
        )
    return rows


def select(rows: list[dict], lane: str) -> list[dict]:
    return [row for row in rows if lane in row["lane"].split(",")]


def load(path: Path = DEFAULT_FILE) -> list[dict]:
    return parse(path.read_text(), str(path))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lane", required=True, choices=["compile", "lsp"])
    parser.add_argument("--urls", action="store_true", help="print name and git URL")
    parser.add_argument("--file", type=Path, default=DEFAULT_FILE)
    args = parser.parse_args()
    for row in select(load(args.file), args.lane):
        print(f"{row['name']}\t{row['url']}" if args.urls else row["name"])


if __name__ == "__main__":
    main()
