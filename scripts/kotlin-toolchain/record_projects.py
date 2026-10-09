#!/usr/bin/env python3
"""Record what JetBrains' `kotlin` command reports for each project case.

A case file holds the project's files and the recorded output, each after a `--- <name>` line:

    --- project.yaml
    modules: [f]
    --- expected
    project.yaml:1:11: ERROR: Unresolved module path `f`
    --- stdout
    ...

A `krusty` section, kept as written, holds what krusty-toolchain reports instead where it
deliberately differs (refusing what it does not implement); the toolchain must report an error too.
Every other section is a file of the project, at that path below the root.
The recorder writes the files to a fresh directory beside the `kotlin` wrapper, runs
`./kotlin show modules`, and rewrites `expected` (one problem per line: the file relative to the
root with its line and column when the toolchain gives them, the severity, and the message with
line breaks written as `\\n`) and `stdout` (the module table, present only when the command succeeds).
The project is written to a directory named `project`, which names its root module.

With `--settings`, the recorder runs `./kotlin show settings --all-modules` instead and records the
settings it prints, with trailing spaces dropped from each line, as `settings` (present only when
the command succeeds). Problems are read from both streams, since warnings go to standard output;
the settings follow them.

With `--dependencies <repository>`, the recorder runs
`./kotlin show dependencies --all-modules --include-tests` and records the graphs it prints, with
trailing spaces dropped from each line, as `dependencies`. The artifacts are read from a local
Maven repository (`repositories: [mavenLocal]`): `<repository>` with the case's `m2/<path>`
sections added. The toolchain gets a fresh cache and no network, so every artifact a case
resolves must be in that repository.

Usage: record_projects.py [--settings | --dependencies <repository>] <kotlin wrapper> <case file>...
The environment must be the one the toolchain runs in (JAVA_HOME, LC_ALL=C.UTF-8, no banner).
"""

import os
import re
import shutil
import subprocess
import sys
import tempfile

RECORDED = ("expected", "stdout", "settings", "dependencies")
# Sections below this directory are artifacts of the case's local Maven repository.
REPOSITORY_SECTION = "m2/"
# Kept as written: what krusty-toolchain reports where it deliberately differs, and why.
KRUSTY = "krusty"
SEVERITY = "(ERROR|WARNING|WEAK WARNING)"
BOXED = re.compile(r"^\s+╭─ " + SEVERITY + r": (.*)$")
BOX_LINE = re.compile(r"^\s+│(?: (.*))?$")
LOCATION = re.compile(r"^→ (.*?)(?::(\d+):(\d+))?$")
PLAIN = re.compile(r"^" + SEVERITY + r": (.*)$")
FILE = re.compile(r"^ ╰→ (.*)$")
# A conflict is printed without a severity; it is an error.
CONFLICT = "Conflicting values for property "
ABORT = (
    "ERROR: Aborting because there were errors in the Kotlin project file, please see above.",
    "ERROR: failed to read Kotlin project model, refer to the errors above",
)


def read_case(path):
    # Lines before the first section describe the case.
    sections = [[None, []]]
    with open(path, encoding="utf-8") as handle:
        for line in handle.read().splitlines():
            if line.startswith("--- "):
                sections.append([line[4:], []])
            else:
                sections[-1][1].append(line)
    return sections


def relative(root, text):
    """`text` with the project root dropped from a path in it."""
    return text.replace(root + os.sep, "")


def parse(root, output):
    """The problems in `kotlin`'s output, in order, and its module table."""
    problems, table, lines, index = [], [], output.splitlines(), 0
    while index < len(lines):
        line = lines[index]
        boxed, plain = BOXED.match(line), PLAIN.match(line)
        if boxed:
            # A boxed problem: its message, then its location, then the quoted source.
            severity, message = boxed.group(1), [boxed.group(2)]
            where = ""
            while index + 1 < len(lines) and BOX_LINE.match(lines[index + 1]):
                index += 1
                text = BOX_LINE.match(lines[index]).group(1)
                location = LOCATION.match(text or "")
                if location:
                    file, row, column = location.groups()
                    where = relative(root, file) + (f":{row}:{column}" if row else "") + ": "
                    break
                if text is not None:
                    message.append(text)
            while not lines[index].lstrip().startswith("╰─"):
                index += 1
            problems.append(f"{where}{severity}: " + "\\n".join(message))
        elif plain and line not in ABORT:
            severity, message = plain.group(1), [plain.group(2)]
            if index + 1 < len(lines) and FILE.match(lines[index + 1]):
                # A problem with a whole file.
                index += 1
                file = relative(root, FILE.match(lines[index]).group(1))
                problems.append(f"{file}: {severity}: {message[0]}")
            else:
                # A problem with the project as a whole: its message runs to the next blank line.
                while index + 1 < len(lines) and lines[index + 1].strip():
                    index += 1
                    message.append(relative(root, lines[index]))
                problems.append(f"{severity}: " + "\\n".join(message))
        elif line.startswith(CONFLICT):
            # Its values and their places run to the next line that does not continue them.
            message = [relative(root, line)]
            while index + 1 < len(lines) and (
                lines[index + 1].startswith(" ")
                or not lines[index + 1].strip()
                and index + 2 < len(lines)
                and lines[index + 2].startswith("  - ")
            ):
                index += 1
                message.append(relative(root, lines[index]))
            problems.append("ERROR: " + "\\n".join(message))
        elif line.startswith("╭─"):
            while True:
                table.append(line)
                if line.startswith("╰"):
                    break
                index += 1
                line = lines[index]
        index += 1
    return problems, table


def without_download_warnings(lines):
    """`lines` without the warnings, and their stack traces, about artifacts the toolchain could not
    download: a case's repository holds metadata only, and the network is unreachable."""
    kept, warning = [], False
    for line in lines:
        if line.startswith("WARN  "):
            warning = True
        elif warning and (line.startswith(("Caused by: ", "\t")) or line.lstrip().startswith(("at ", "... "))):
            pass
        else:
            warning = False
            kept.append(line)
    return kept


def write(directory, name, lines):
    file = os.path.join(directory, name)
    os.makedirs(os.path.dirname(file), exist_ok=True)
    with open(file, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + ("\n" if lines else ""))


def record(wrapper, path, mode, repository):
    settings = mode == "settings"
    sections = read_case(path)
    with tempfile.TemporaryDirectory(prefix="kotlin-project-case-") as directory:
        directory = os.path.realpath(directory)
        # A fixed directory name, since the root module is named after it.
        root = os.path.join(directory, "project")
        os.makedirs(root)
        local = os.path.join(directory, "m2")
        if repository is not None:
            shutil.copytree(repository, local)
        for name, lines in sections:
            if name is None or name in RECORDED or name == KRUSTY:
                continue
            if name.startswith(REPOSITORY_SECTION):
                write(local, name[len(REPOSITORY_SECTION):], lines)
            else:
                write(root, name, lines)
        shutil.copy(wrapper, os.path.join(root, "kotlin"))
        environment = dict(os.environ)
        if mode == "dependencies":
            command = ["show", "dependencies", "--all-modules", "--include-tests"]
            # The case's repository, a fresh cache, and an unreachable proxy for everything else.
            environment["JAVA_TOOL_OPTIONS"] = " ".join([
                environment.get("JAVA_TOOL_OPTIONS", ""),
                f"-Dmaven.repo.local={local}",
                "-Dhttps.proxyHost=127.0.0.1 -Dhttps.proxyPort=9",
            ])
            environment["KOTLIN_SHARED_CACHE_DIR"] = os.path.join(directory, "cache")
        elif settings:
            command = ["show", "settings", "--all-modules"]
        else:
            command = ["show", "modules"]
        run = subprocess.run(
            ["./kotlin", *command],
            cwd=root,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=600,
        )
        printed = [
            line for line in run.stdout.splitlines()
            if not line.startswith("Picked up JAVA_TOOL_OPTIONS")
        ]
        output = "\n".join(printed)
        problems, table = parse(root, output)
        if run.returncode != 0:
            stdout = None
        elif mode == "dependencies":
            lines = [line.rstrip() for line in without_download_warnings(printed)]
            first = next(
                index for index, line in enumerate(lines)
                if line.startswith("Dependencies of module ")
            )
            stdout = lines[first:]
            while stdout and not stdout[-1]:
                stdout.pop()
        elif settings:
            lines = [line.rstrip() for line in printed]
            first = next(
                index for index, line in enumerate(lines)
                if line.startswith(("Module: ", "settings@"))
            )
            stdout = lines[first:]
        else:
            stdout = table
    kept = [section for section in sections if section[0] not in RECORDED]
    kept.append(["expected", problems])
    if stdout is not None:
        kept.append(["stdout" if mode == "modules" else mode, stdout])
    with open(path, "w", encoding="utf-8") as handle:
        for name, lines in kept:
            if name is not None:
                handle.write(f"--- {name}\n")
            for line in lines:
                handle.write(line + "\n")


def main():
    arguments = sys.argv[1:]
    mode, repository = "modules", None
    if arguments[:1] == ["--settings"]:
        mode, arguments = "settings", arguments[1:]
    elif arguments[:1] == ["--dependencies"]:
        mode, repository, arguments = "dependencies", os.path.realpath(arguments[1]), arguments[2:]
    wrapper, cases = arguments[0], arguments[1:]
    for case in cases:
        record(wrapper, case, mode, repository)
        print(case, file=sys.stderr)


if __name__ == "__main__":
    main()
