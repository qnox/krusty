#!/usr/bin/env python3
"""Moves one build's compile outputs out of a corpus checkout, driven by the compile-task inventory
`corpus.init.gradle` recorded for that build.

usage: outputs.py [--keep] <compilations manifest> <Gradle root> <snapshot dir>
       outputs.py --clear <compilations manifest> <Gradle root> <snapshot dir> <changes file>

Each manifest line is `<task path>\t<output directory relative to the Gradle root>`. Every listed
directory is validated as task-owned (relative, inside the Gradle root, neither the root itself nor
nested in or shared with another listed directory), copied to the same relative path under the
snapshot dir, and then removed from the checkout, so the next build starts without it. Any invalid
entry fails the whole snapshot before anything is touched.

With `--keep` the directories are copied and left in place, for a later build that must run on
them. `--clear` then removes them, after listing in the changes file every path under them that
differs from the snapshot (written, rewritten or deleted since), one per line relative to the
Gradle root; an empty file proves the later build ran on exactly the snapshotted outputs.
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


def snapshot(manifest, root, out, keep=False):
    """Copy every listed output directory to `out` and, unless `keep`, remove it from the checkout."""
    entries = validated(inventory(manifest), root)
    if os.path.exists(out):
        shutil.rmtree(out)
    os.makedirs(out)
    for _, rel, path in entries:
        if os.path.isdir(path):
            shutil.copytree(path, os.path.join(out, rel), symlinks=True)
            if not keep:
                shutil.rmtree(path)
        elif os.path.lexists(path):
            raise InventoryError(f'output directory {rel!r} is not a directory')
    return entries


def files(directory):
    """{path relative to `directory`: absolute path} of every file under it."""
    found = {}
    for parent, _, names in os.walk(directory):
        for name in names:
            path = os.path.join(parent, name)
            found[os.path.relpath(path, directory)] = path
    return found


def same_file(left, right):
    with open(left, 'rb') as a, open(right, 'rb') as b:
        return a.read() == b.read()


def clear(manifest, root, out):
    """Remove every listed output directory from the checkout, and return the paths under them,
    relative to the Gradle root, that differ from the snapshot in `out`."""
    entries = validated(inventory(manifest), root)
    changed = []
    for _, rel, path in entries:
        now, kept = files(path), files(os.path.join(out, rel))
        for name in sorted(set(now) | set(kept)):
            if name not in now or name not in kept or not same_file(now[name], kept[name]):
                changed.append(os.path.join(rel, name))
        if os.path.isdir(path):
            shutil.rmtree(path)
        elif os.path.lexists(path):
            raise InventoryError(f'output directory {rel!r} is not a directory')
    return changed


def main():
    args = sys.argv[1:]
    try:
        if args[0] == '--clear':
            manifest, root, out, changes = args[1:5]
            changed = clear(manifest, root, out)
            with open(changes, 'w') as f:
                f.writelines(path + '\n' for path in changed)
        else:
            keep = args[0] == '--keep'
            manifest, root, out = args[1:4] if keep else args[0:3]
            snapshot(manifest, root, out, keep)
    except (InventoryError, OSError) as e:
        print(f'outputs.py: {e}', file=sys.stderr)
        sys.exit(1)


if __name__ == '__main__':
    main()
