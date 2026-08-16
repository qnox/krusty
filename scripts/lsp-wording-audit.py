#!/usr/bin/env python3
"""Compare krusty's diagnostic wording against the Kotlin compiler's own message templates.

The reference wording is not a document we can read: it is the compiled `FirErrorsDefaultMessages`
map shipped inside the IntelliJ Kotlin LSP distribution. This script reads the templates straight
out of the jar's class constant pool -- no LSP server, no JVM, no network -- and diffs them against
every message literal krusty passes to `Diagnostics::error`/`warn`.

Usage:
    scripts/lsp-wording-audit.py --lsp-dist /path/to/kotlin-lsp [--src src] [--json out.json]

Exit status is 0 when the audit runs; it is a report, not a gate.
"""

import argparse
import difflib
import json
import pathlib
import re
import struct
import sys
import zipfile

MESSAGE_CLASSES = re.compile(
    r"(FirErrorsDefaultMessages|FirSyntaxErrorsDefaultMessages|Fir\w+ErrorsDefaultMessages)\.class$"
)
EMIT_CALL = re.compile(r"\.(?:error|warn|error_with_identity|warn_with_identity)\s*\(")
STRING_LITERAL = re.compile(r'"((?:[^"\\]|\\.)*)"')
PLACEHOLDER = re.compile(r"\{[^}]*\}")


def constant_pool_strings(class_bytes):
    """Yield every CONSTANT_Utf8 entry of a class file.

    A jar is arbitrary input, so a truncated or otherwise unreadable entry ends the walk rather
    than raising: one bad class must not abort the audit.
    """
    if len(class_bytes) < 10 or class_bytes[:4] != b"\xca\xfe\xba\xbe":
        return
    count = struct.unpack(">H", class_bytes[8:10])[0]
    offset, index = 10, 1
    while index < count:
        if offset >= len(class_bytes):
            return
        tag = class_bytes[offset]
        offset += 1
        if tag == 1:
            if offset + 2 > len(class_bytes):
                return
            length = struct.unpack(">H", class_bytes[offset : offset + 2])[0]
            offset += 2
            if offset + length > len(class_bytes):
                return
            yield class_bytes[offset : offset + length].decode("utf-8", "replace")
            offset += length
        elif tag in (7, 8, 16, 19, 20):
            offset += 2
        elif tag == 15:
            offset += 3
        elif tag in (3, 4, 9, 10, 11, 12, 17, 18):
            offset += 4
        elif tag in (5, 6):  # long/double take two constant-pool slots
            offset += 8
            index += 1
        else:
            return
        index += 1


def reference_templates(dist):
    """Every diagnostic message template the Kotlin frontend can render."""
    templates = set()
    for jar in sorted(pathlib.Path(dist).rglob("*.jar")):
        try:
            archive = zipfile.ZipFile(jar)
        except (zipfile.BadZipFile, OSError):
            continue
        for entry in archive.namelist():
            if not MESSAGE_CLASSES.search(entry):
                continue
            for text in constant_pool_strings(archive.read(entry)):
                if len(text) > 8 and " " in text and not text.startswith("("):
                    # `''` is the MessageFormat escape for a literal quote.
                    templates.add(re.sub(r"\{\d+\}", "{}", text).replace("''", "'"))
    return templates


RAW_STRING_OPEN = re.compile(r'r(#*)"')


def balanced_call(text, open_paren):
    """The source text of the call whose opening parenthesis is at `open_paren`.

    Parentheses inside string literals do not nest, so both ordinary and raw strings are skipped
    whole; a raw string ends only at a quote followed by as many `#` as opened it, and a `"` inside
    one is just a character.
    """
    depth, index = 0, open_paren
    while index < len(text):
        raw = RAW_STRING_OPEN.match(text, index)
        if raw:
            close = f'"{raw.group(1)}'
            end = text.find(close, raw.end())
            index = len(text) if end < 0 else end + len(close)
            continue
        char = text[index]
        if char == '"':
            index += 1
            while index < len(text) and text[index] != '"':
                index += 2 if text[index] == "\\" else 1
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return text[open_paren : index + 1]
        index += 1
    return None


def krusty_messages(sources):
    """Message literals krusty reports, mapped to the sites that report them."""
    sites = {}
    for path in sorted(pathlib.Path(sources).rglob("*.rs")):
        text = path.read_text()
        for call in EMIT_CALL.finditer(text):
            argument = balanced_call(text, call.end() - 1)
            if not argument:
                continue
            line = text.count("\n", 0, call.start()) + 1
            for literal in STRING_LITERAL.finditer(argument):
                message = literal.group(1)
                if len(message) < 6 or " " not in message:
                    continue
                sites.setdefault(message, []).append(f"{path}:{line}")
    return sites


def sentence_case(message):
    """krusty stores messages lowercase-first; the LSP boundary capitalizes them."""
    normalized = PLACEHOLDER.sub("{}", message).strip()
    return normalized[:1].upper() + normalized[1:]


def audit(dist, sources):
    templates = reference_templates(dist)
    matched, gaps = [], []
    for message, sites in krusty_messages(sources).items():
        rendered = sentence_case(message)
        if rendered in templates:
            matched.append({"message": rendered, "sites": sites})
            continue
        closest = difflib.get_close_matches(rendered, templates, n=1, cutoff=0.5)
        gaps.append(
            {
                "krusty": rendered,
                "reference": closest[0] if closest else None,
                "similarity": (
                    round(difflib.SequenceMatcher(None, rendered, closest[0]).ratio(), 3)
                    if closest
                    else 0.0
                ),
                "sites": sites,
            }
        )
    gaps.sort(key=lambda gap: -gap["similarity"])
    return {"templates": len(templates), "matched": matched, "gaps": gaps}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lsp-dist", required=True, help="unpacked kotlin-lsp distribution")
    parser.add_argument("--src", default="src", help="krusty source root to audit")
    parser.add_argument("--json", help="write the full report here")
    parser.add_argument("--top", type=int, default=25, help="how many gaps to print")
    arguments = parser.parse_args()

    report = audit(arguments.lsp_dist, arguments.src)
    if not report["templates"]:
        print("no reference templates found -- is --lsp-dist an unpacked kotlin-lsp?", file=sys.stderr)
        return 1
    total = len(report["matched"]) + len(report["gaps"])
    print(f"{report['templates']} reference templates")
    print(f"{len(report['matched'])}/{total} krusty diagnostics match a template exactly")
    for gap in report["gaps"][: arguments.top]:
        print(f"\n{gap['sites'][0]} (x{len(gap['sites'])})  similarity {gap['similarity']}")
        print(f"  krusty   : {gap['krusty']}")
        print(f"  reference: {gap['reference']}")
    if arguments.json:
        pathlib.Path(arguments.json).write_text(json.dumps(report, indent=2))
        print(f"\nfull report: {arguments.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
