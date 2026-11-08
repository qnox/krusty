#!/usr/bin/env python3
"""Render generated release-note PR titles literally, preserving author attribution."""

import json
import re
import sys


# GitHub's generated change bullets end with the author and the PR URL. Match the final
# suffix, since arbitrary title text may itself contain "by @someone in ...".
CHANGE = re.compile(
    r"^(?P<bullet>[*+-] )(?P<title>.+)(?P<suffix> by "
    r"@[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:\[bot\])? in "
    r"https://github\.com/[^/\s]+/[^/\s]+/pull/[0-9]+)(?P<end>\r?\n?)$"
)


def literal_title(title):
    """A code span cannot turn title mentions or Markdown into active markup."""
    width = max((len(run) for run in re.findall(r"`+", title)), default=0) + 1
    fence = "`" * width
    # CommonMark removes one surrounding space unless the content is entirely spaces.
    padding = " " if title.strip(" ") else ""
    return f"{fence}{padding}{title}{padding}{fence}"


def format_notes(body):
    result = []
    for line in body.splitlines(keepends=True):
        change = CHANGE.fullmatch(line)
        if change:
            line = (change["bullet"] + literal_title(change["title"])
                    + change["suffix"] + change["end"])
        result.append(line)
    return "".join(result)


if __name__ == "__main__":
    body = json.load(sys.stdin)["body"]
    if not isinstance(body, str):
        raise ValueError("generated release notes must contain a text body")
    sys.stdout.write(format_notes(body))
