#!/usr/bin/env python3
"""Lists and compares the Kotlin/JVM compile actions of a Bazel corpus project (`bazel.sh`).

usage: bazel_compare.py snapshot <execution log> <execution root> <compilations.tsv> <output dir>
       bazel_compare.py compare <work dir> <name> <reference build exit code> <krusty build exit code>

`snapshot` reads one build's JSON execution log (`--execution_log_json_file`), lists every compile
action it ran (a `JvmCompile`, the project's own builder, or a `KrustyCompile`) with the jar it
writes, how long it ran, its exit code, and for a `JvmCompile` what keeps it from krusty, and copies
each jar that exists out of the execution root's `bazel-out` (`bazel info execution_root`; the
project may name its convenience symlinks anything) to `<output dir>`, under its path below
`bazel-out`.

`compare` compares the jars of every compile action the reference build listed, entry by entry for
the class files, and writes <work dir>/out/<name>/compare.json in the shape `compare.py` writes for a
Gradle project, so `summarize.py` and the site's metrics read it unchanged. Each compile action is one
compilation, named by its target's label, followed by ` [for tool]` when it builds the target for
the execution platform, as Bazel's own progress messages name it: a target that is both a tool and a
library of the build is compiled once in each configuration. An action the krusty build left on the project's builder
is a miss with its reason (`fallback`), and a krusty action that failed is a miss with the first error
the build log shows for it. There is no verify or test build.
"""
import json
import os
import re
import shutil
import sys
import zipfile

import compare as gradle

COMPILE = ('JvmCompile', 'KrustyCompile')
FIELDS = ('label', 'jar', 'mnemonic', 'seconds', 'exit', 'reason')


def spawns(path):
    """The spawns of a JSON execution log: a stream of concatenated JSON objects."""
    decoder = json.JSONDecoder()
    with open(path) as f:
        text = f.read()
    at = 0
    while True:
        while at < len(text) and text[at].isspace():
            at += 1
        if at == len(text):
            return
        spawn, at = decoder.raw_decode(text, at)
        yield spawn


def duration(value):
    """Seconds of a protobuf JSON duration (`"1.250s"`), or None."""
    if isinstance(value, str) and value.endswith('s'):
        try:
            return round(float(value[:-1]), 2)
        except ValueError:
            return None
    return None


def held_back(inputs):
    """Why the krusty integration left a compile action on the project's builder, from its inputs:
    Java sources without Kotlin ones, source jars, or else build-supplied compiler plugins, the
    integration's three conditions. A mixed Kotlin and Java target is krusty's."""
    if any(path.endswith('.srcjar') for path in inputs):
        return 'source jars'
    if (any(path.endswith('.java') for path in inputs)
            and not any(path.endswith('.kt') for path in inputs)):
        return 'Java-only target'
    return 'compiler plugins'


def compile_actions(log):
    """{name: {jar, mnemonic, seconds, exit, reason}} for the compile actions of an execution log,
    named by `action_name`. The jar is the output named after the target (`<name>.jar`), as a path
    below the execution root."""
    found = {}
    for spawn in spawns(log):
        mnemonic = spawn.get('mnemonic')
        if mnemonic not in COMPILE:
            continue
        label = spawn['targetLabel']
        jar_name = '/' + label.rsplit(':', 1)[-1] + '.jar'
        jars = [path for path in spawn.get('listedOutputs', []) if path.endswith(jar_name)]
        if len(jars) != 1:
            raise ValueError(f'{log}: {label}: expected one {jar_name[1:]} output, found {jars}')
        name = action_name(label, jars[0])
        if name in found:
            raise ValueError(f'{log}: {name}: compiled twice')
        inputs = [entry.get('path', '') for entry in spawn.get('inputs', [])]
        found[name] = dict(
            jar=jars[0], mnemonic=mnemonic,
            seconds=duration(spawn.get('walltime')) or duration(spawn.get('metrics', {}).get('totalTime')),
            exit=int(spawn.get('exitCode', 0)),
            reason=held_back(inputs) if mnemonic == 'JvmCompile' else None)
    return found


def output_relative(path):
    """`bazel-out/<configuration>/bin/<path>` as `<configuration>/bin/<path>`."""
    parts = path.split('/')
    if len(parts) > 3 and parts[0] == 'bazel-out' and parts[2] == 'bin':
        return '/'.join(parts[1:])
    raise ValueError(f'not a bazel-out/<configuration>/bin path: {path}')


def action_name(label, jar):
    """The target's label, followed by ` [for tool]` when the jar is built in an execution
    configuration (`bazel-out/<platform>-<mode>-exec/...`)."""
    configuration = output_relative(jar).split('/', 1)[0]
    return label + (' [for tool]' if configuration.endswith('-exec') else '')


def snapshot(log, execroot, manifest, out):
    """Lists the compile actions of one build and copies their jars out of the execution root."""
    actions = compile_actions(log)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    with open(manifest, 'w') as f:
        for label, action in sorted(actions.items()):
            source = os.path.join(execroot, action['jar'])
            if os.path.isfile(source):
                target = os.path.join(out, output_relative(action['jar']))
                os.makedirs(os.path.dirname(target), exist_ok=True)
                shutil.copyfile(source, target)
            fields = [label] + ['' if action[k] is None else str(action[k]) for k in FIELDS[1:]]
            f.write('\t'.join(fields) + '\n')


def read_manifest(path):
    """{label: {jar, mnemonic, seconds, exit, reason}} from a snapshot's manifest; {} without one."""
    found = {}
    try:
        with open(path) as f:
            lines = [line.rstrip('\n').split('\t') for line in f if line.strip()]
    except OSError:
        return found
    for fields in lines:
        row = dict(zip(FIELDS, fields))
        found[row['label']] = dict(jar=row['jar'], mnemonic=row['mnemonic'],
                                   seconds=float(row['seconds']) if row['seconds'] else None,
                                   exit=int(row['exit']), reason=row['reason'] or None)
    return found


def classes(jar):
    """{entry name: bytes} of a jar's class files; {} when the jar does not exist."""
    if not os.path.isfile(jar):
        return {}
    with zipfile.ZipFile(jar) as z:
        return {n: z.read(n) for n in z.namelist() if n.endswith('.class')}


FAILED = re.compile(r'^ERROR: .*?: compile (\S+) with krusty (?:\([^)]*\) )?(\[for tool\] )?failed: ?(.*)$')


def failures(log_path, checkout):
    """{label: message} for the krusty compile actions the build log reports failed: the first line of
    the action's output that reads as an error, else the failure Bazel reports. Paths inside the
    checkout are shown relative to it."""
    found = {}
    try:
        with open(log_path, errors='replace') as f:
            lines = f.read().splitlines()
    except OSError:
        return found
    prefixes = ('file://' + checkout + '/', checkout + '/')
    for i, line in enumerate(lines):
        header = FAILED.match(line)
        if not header:
            continue
        name = header.group(1) + (' [for tool]' if header.group(2) else '')
        if name in found:
            continue
        message = header.group(3).strip()
        for detail in lines[i + 1:]:
            if detail.startswith(('ERROR: ', 'INFO: ', 'WARNING: ', 'Target ')):
                break
            if gradle.ERROR_LINE.search(detail):
                message = detail.strip()
                break
        for prefix in prefixes:
            message = message.replace(prefix, '')
        found[name] = message[:300]
    return found


def build_failure(log_path):
    """The first `ERROR:` line of a Bazel log that no compile action explains, or None."""
    try:
        with open(log_path, errors='replace') as f:
            lines = f.read().splitlines()
    except OSError:
        return None
    for line in lines:
        if line.startswith('ERROR: ') and not FAILED.match(line):
            return line[len('ERROR: '):].strip()[:300]
    return None


def compare(work, name):
    """The compared compilations, and the listed compile actions whose reference jar holds no class."""
    out, logs = os.path.join(work, 'out', name), os.path.join(work, 'logs')
    checkout = os.path.join(work, 'src', name)
    reference = read_manifest(os.path.join(logs, f'{name}.kotlinc.compilations.tsv'))
    candidate = read_manifest(os.path.join(logs, f'{name}.krusty.compilations.tsv'))
    errors = failures(os.path.join(logs, f'{name}.krusty.log'), checkout)
    not_run = 'not run: a target it depends on failed'
    if not candidate:
        failure = build_failure(os.path.join(logs, f'{name}.krusty.log'))
        if failure:
            not_run = 'not run: the krusty build failed: ' + failure
    rows, empty = [], []
    for label, action in sorted(reference.items()):
        rel = output_relative(action['jar'])
        theirs = classes(os.path.join(out, 'kotlinc', rel))
        ours_action = candidate.get(label)
        ours = classes(os.path.join(out, 'krusty', rel)) if ours_action else {}
        if not theirs:
            empty.append(dict(task=label, dir=rel, krusty_classes=len(ours)))
            continue
        fallback = ours_action['reason'] if ours_action and ours_action['mnemonic'] == 'JvmCompile' else None
        built = (fallback is None and ours_action is not None and ours_action['exit'] == 0 and bool(ours))
        error = None
        if not built and fallback is None:
            if ours_action is None:
                error = not_run
            else:
                error = errors.get(label, 'krusty failed without an error in the log'
                                   if ours_action['exit'] else 'krusty wrote no class')
        rows.append(dict(task=label, dir=rel, built=built, fallback=fallback, error=error,
                         classes=len(theirs),
                         identical=sum(1 for k, v in theirs.items() if ours.get(k) == v) if built else 0,
                         only_kotlinc=len(set(theirs) - set(ours)) if built else None,
                         only_krusty=len(set(ours) - set(theirs)) if built else None,
                         seconds=dict(kotlinc=action['seconds'],
                                      krusty=ours_action['seconds'] if ours_action else None),
                         verify=None))
    return rows, empty


def main():
    if sys.argv[1:2] == ['snapshot'] and len(sys.argv) == 6:
        snapshot(*sys.argv[2:6])
        return
    if sys.argv[1:2] != ['compare'] or len(sys.argv) != 6:
        sys.exit(__doc__)
    work, name, reference_exit, krusty_exit = sys.argv[2], sys.argv[3], int(sys.argv[4]), int(sys.argv[5])
    rows, empty = compare(work, name)
    summary = dict(
        name=name,
        reference_exit=reference_exit,
        verify_exit=None,
        loc=gradle.lines_of_code(os.path.join(work, 'src', name)),
        compilations=len(rows),
        built=sum(r['built'] for r in rows),
        classes=sum(r['classes'] for r in rows),
        identical=sum(r['identical'] for r in rows),
        cost={mode: gradle.build_cost(work, name, mode, rows) for mode in ('kotlinc', 'krusty')},
        hotspots=None,
        tests=None,
        rows=rows,
        no_reference_classes=empty,
    )
    with open(os.path.join(work, 'out', name, 'compare.json'), 'w') as f:
        json.dump(summary, f, indent=1)
    print(f"{name}: reference exit {reference_exit}, krusty exit {krusty_exit}; compilations "
          f"{summary['built']}/{summary['compilations']} built by krusty; classes identical "
          f"{summary['identical']}/{summary['classes']}")
    for r in rows:
        state = 'ok ' if r['built'] else ('KTC' if r['fallback'] else 'ERR')
        print(f"  {state} {r['task']}: {r['identical']}/{r['classes']}"
              + (f" left to the project's builder: {r['fallback']}" if r['fallback'] else '')
              + (f" error: {r['error']}" if r['error'] else '')
              + (f" only_kotlinc={r['only_kotlinc']} only_krusty={r['only_krusty']}" if r['built'] else ''))


if __name__ == '__main__':
    main()
