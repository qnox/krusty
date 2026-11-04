#!/usr/bin/env python3
"""Build the runner controller's route request from a workflow file.

The route job asks the runner controller where each job of this run should run. This script
reads the workflow and describes every job the router may place: its name as GitHub shows it,
its matrix rows, the jobs it needs, and the labels it may get. The workflow stays the only
source of those facts.

A job is left out, and named on stderr, when the router cannot describe it before the run
starts: a job with an `if:` other than `always()`, a matrix built from an output this script
was not given, a matrix value that is not a scalar, or a name or `runs-on:` it cannot read.
Jobs left out keep their own `runs-on:`.

Usage: route-request.py WORKFLOW --asking JOB [--output JOB.OUTPUT=JSON ...]

--asking names the route job itself: it is already running, so it is not placed.
"""

import argparse
import itertools
import json
import re
import sys

import yaml

HOSTED = ["ubuntu-latest"]
# `fromJSON('["self-hosted", "rust-4c-8gb"]')`: the labels a same-repo run gets; a fork PR gets
# ubuntu-latest from the other branch of the expression and never asks the router.
FROM_JSON = re.compile(r"""fromJSON\('(\[[^']*\])'\)""", re.IGNORECASE)
OUTPUT = re.compile(r"^\$\{\{\s*fromJson\(needs\.([\w-]+)\.outputs\.([\w-]+)\)\s*\}\}$", re.IGNORECASE)
MATRIX_NAME = re.compile(r"^(.+?) \(((?:\$\{\{ matrix\.\w+ \}\}(?:, )?)+)\)$")
MATRIX_REF = re.compile(r"\$\{\{ matrix\.(\w+) \}\}")


class Skip(Exception):
    """The router cannot describe this job; it keeps its own runs-on."""


def labels(runs_on):
    if isinstance(runs_on, list) and all(isinstance(x, str) for x in runs_on):
        return runs_on
    if isinstance(runs_on, str) and "${{" not in runs_on:
        return [runs_on]
    if isinstance(runs_on, str):
        found = FROM_JSON.findall(runs_on)
        if len(found) == 1:
            parsed = json.loads(found[0])
            if isinstance(parsed, list) and parsed and all(isinstance(x, str) for x in parsed):
                return parsed
    raise Skip(f"cannot read runs-on {runs_on!r}")


def values(value, outputs):
    """A matrix key's values: a list, or an output this run already knows."""
    if isinstance(value, str):
        m = OUTPUT.match(value.strip())
        if not m or f"{m[1]}.{m[2]}" not in outputs:
            raise Skip(f"matrix value {value!r} is not a given output")
        value = outputs[f"{m[1]}.{m[2]}"]
    if not isinstance(value, list):
        raise Skip(f"matrix value {value!r} is not a list")
    if not all(isinstance(v, (str, int, float, bool)) for v in value):
        raise Skip("a matrix value is not a scalar")
    return value


def matches(row, entry):
    return all(row.get(k) == v for k, v in entry.items())


def rows(matrix, outputs):
    """GitHub's matrix expansion: the cross product, less `exclude`, then `include`."""
    if isinstance(matrix, str):
        raise Skip(f"matrix {matrix!r} is an expression")
    base = [k for k in matrix if k not in ("include", "exclude")]
    lists = [values(matrix[k], outputs) for k in base]
    combos = [dict(zip(base, c)) for c in itertools.product(*lists)] if base else []
    combos = [c for c in combos if not any(matches(c, e) for e in matrix.get("exclude", []))]
    originals = [dict(c) for c in combos]
    for entry in matrix.get("include", []):
        if not all(isinstance(v, (str, int, float, bool)) for v in entry.values()):
            raise Skip("an include value is not a scalar")
        # An include extends every combination whose original values it does not change; one
        # that extends none is a combination of its own.
        fits = [i for i, o in enumerate(originals) if all(o.get(k, v) == v for k, v in entry.items())]
        for i in fits:
            combos[i].update(entry)
        if not fits:
            combos.append(dict(entry))
    keys = list(dict.fromkeys(k for c in combos for k in c))
    return keys, combos


def job_class(key, job, outputs):
    if job.get("if") not in (None, "always()", "${{ always() }}"):
        raise Skip(f"runs only if {job['if']}")
    default = labels(job.get("runs-on"))
    needs = job.get("needs", [])
    needs = [needs] if isinstance(needs, str) else needs
    matrix = job.get("strategy", {}).get("matrix")
    name = job.get("name", key)
    out = {"key": key, "name": name, "default": default, "hosted": HOSTED, "needs": needs}
    if default != HOSTED:
        out["self_allowed"] = [default]
    if matrix is None:
        if "${{" in name:
            raise Skip(f"cannot read name {name!r}")
        return out
    keys, combos = rows(matrix, outputs)
    if "name" in job:
        m = MATRIX_NAME.match(name)
        if not m:
            raise Skip(f"cannot read name {name!r}")
        out["name"] = m[1]
        keys = MATRIX_REF.findall(m[2])
    out["matrix_keys"] = keys
    out["rows"] = combos
    return out


def request(workflow, outputs, asking=None):
    classes = []
    for key, job in workflow["jobs"].items():
        if key == asking:
            continue
        try:
            classes.append(job_class(key, job, outputs))
        except Skip as e:
            print(f"route: {key} keeps its runs-on: {e}", file=sys.stderr)
    # A job that waits for one left out cannot be timed either.
    while True:
        keys = {c["key"] for c in classes}
        waiting = [c for c in classes if any(n not in keys for n in c["needs"])]
        if not waiting:
            return {"classes": classes}
        for c in waiting:
            missing = ", ".join(n for n in c["needs"] if n not in keys)
            print(f"route: {c['key']} keeps its runs-on: needs {missing}, which is left out", file=sys.stderr)
        classes = [c for c in classes if c not in waiting]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("workflow")
    parser.add_argument("--asking", required=True, metavar="JOB")
    parser.add_argument("--output", action="append", default=[], metavar="JOB.OUTPUT=JSON")
    args = parser.parse_args()
    outputs = {}
    for item in args.output:
        name, _, value = item.partition("=")
        outputs[name] = json.loads(value)
    with open(args.workflow) as f:
        workflow = yaml.safe_load(f)
    json.dump(request(workflow, outputs, args.asking), sys.stdout)
    print()


if __name__ == "__main__":
    main()
