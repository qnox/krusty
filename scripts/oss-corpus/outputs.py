#!/usr/bin/env python3
"""Moves one build's compile outputs out of a corpus checkout, driven by the compile-task inventory
`corpus.init.gradle` recorded for that build.

usage: outputs.py <compilations manifest> <Gradle root> <snapshot dir>

Each manifest line is `<task path>\t<output directory relative to the Gradle root>`. Every listed
directory is validated as task-owned (relative, inside the Gradle root, neither the root itself nor
nested in or shared with another listed directory), copied to the same relative path under the
snapshot dir, and then removed from the checkout, so the next build starts without it. Any invalid
entry fails the whole snapshot before anything is touched.
"""
import os
import shutil
import sys


class InventoryError(Exception):
    pass


def inventory(manifest):
    """The (task path, relative output directory) rows of a manifest."""
    rows = []
    with open(manifest) as f:
        for number, line in enumerate(f, 1):
            line = line.rstrip('\n')
            if not line.strip():
                continue
            fields = line.split('\t')
            if len(fields) != 2 or not all(fields):
                raise InventoryError(f'{manifest}:{number}: expected <task>\\t<directory>: {line!r}')
            rows.append((fields[0], fields[1]))
    return rows


def validated(rows, root):
    """Each row's output directory as an absolute path, after checking that it is task-owned."""
    real_root = os.path.realpath(root)
    owners = {}
    for task, rel in rows:
        if os.path.isabs(rel) or rel != os.path.normpath(rel) or rel in ('.', '') \
                or rel.split(os.sep)[0] == '..':
            raise InventoryError(f'{task}: output directory {rel!r} is not a path inside the Gradle root')
        resolved = os.path.realpath(os.path.join(root, rel))
        if os.path.commonpath([real_root, resolved]) != real_root or resolved == real_root:
            raise InventoryError(f'{task}: output directory {rel!r} resolves outside the Gradle root')
        if resolved in owners:
            raise InventoryError(f'{task}: output directory {rel!r} is shared with {owners[resolved]}')
        owners[resolved] = task
    for path, task in owners.items():
        for other, other_task in owners.items():
            if other != path and os.path.commonpath([path, other]) == path:
                raise InventoryError(f'{other_task}: output directory nests inside the one of {task}')
    return [(task, rel, os.path.join(root, rel)) for task, rel in rows]


def snapshot(manifest, root, out):
    """Copy every listed output directory to `out` and remove it from the checkout."""
    entries = validated(inventory(manifest), root)
    if os.path.exists(out):
        shutil.rmtree(out)
    os.makedirs(out)
    for _, rel, path in entries:
        if os.path.isdir(path):
            shutil.copytree(path, os.path.join(out, rel), symlinks=True)
            shutil.rmtree(path)
        elif os.path.lexists(path):
            raise InventoryError(f'output directory {rel!r} is not a directory')
    return entries


def main():
    manifest, root, out = sys.argv[1:4]
    try:
        snapshot(manifest, root, out)
    except (InventoryError, OSError) as e:
        print(f'outputs.py: {e}', file=sys.stderr)
        sys.exit(1)


if __name__ == '__main__':
    main()
