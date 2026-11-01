#!/usr/bin/env python3
"""Rewrites a corpus project's Kotlin version pin to an exact version.

usage: pin.py <file> <key> <version>

`<file>` is a `.toml` version catalog (`<key> = "<version>"`), a `.properties` file
(`<key>=<version>`) or a Kotlin source file (`[const] val <key> = "<version>"`). Exactly one line
must assign `<key>`; no such line, or more than one, is an error and leaves the file unchanged. The
file is read again afterwards to verify that the key now holds `<version>`.
"""
import re
import sys


def assignment(path, key):
    """One assignment of `key`, matched as (prefix, value, suffix)."""
    if path.endswith('.toml'):
        return re.compile(r'^(' + re.escape(key) + r'\s*=\s*")([^"]*)(".*)$')
    if path.endswith('.properties'):
        return re.compile(r'^(' + re.escape(key) + r'\s*[=:]\s*)(\S*)(\s*)$')
    if path.endswith('.kt'):
        return re.compile(r'^(\s*(?:const\s+)?val\s+' + re.escape(key) + r'\s*=\s*")([^"]*)(".*)$')
    raise ValueError(f'unsupported Kotlin version pin file: {path}')


def the_assignment(path, key, lines):
    regex = assignment(path, key)
    found = [(i, m) for i, m in ((i, regex.match(line)) for i, line in enumerate(lines)) if m]
    if len(found) != 1:
        raise ValueError(f'{path}: expected exactly one assignment of {key!r}, found {len(found)}')
    return found[0]


def rewrite(path, key, version):
    with open(path) as f:
        lines = f.read().split('\n')
    index, match = the_assignment(path, key, lines)
    lines[index] = match.group(1) + version + match.group(3)
    with open(path, 'w') as f:
        f.write('\n'.join(lines))
    with open(path) as f:
        _, match = the_assignment(path, key, f.read().split('\n'))
    if match.group(2) != version:
        raise ValueError(f'{path}: {key!r} reads {match.group(2)!r} after the rewrite, not {version!r}')


def main():
    path, key, version = sys.argv[1:4]
    try:
        rewrite(path, key, version)
    except (ValueError, OSError) as e:
        print(f'pin.py: {e}', file=sys.stderr)
        sys.exit(1)


if __name__ == '__main__':
    main()
