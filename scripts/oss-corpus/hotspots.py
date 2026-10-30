#!/usr/bin/env python3
"""Where krusty spends its CPU time in a corpus build, from the build's sampled call stacks.

usage: hotspots.py fold < perf script output > <folded stacks>

`fold` reads `perf script` output of the krusty processes and writes one line per distinct call
stack, root first, frames joined by `;`, then its sample count: the format flame graph tools read.
`compare.py` ranks the functions of those stacks with `rank`.
"""
import collections
import re
import sys

# A legacy-mangled Rust symbol ends in its hash; it would split one function across builds.
RUST_HASH = re.compile(r'::h[0-9a-f]{16}$')
# `perf script` callchain line: address, symbol with offset, then the object in parentheses.
FRAME = re.compile(r'^\s*[0-9a-f]+\s+(.*?)(?:\+0x[0-9a-f]+)?\s+\((.*)\)$')


def frame_name(symbol, dso):
    if symbol and symbol != '[unknown]':
        return RUST_HASH.sub('', symbol)
    return f"[{dso.rsplit('/', 1)[-1]}]" if dso else '[unknown]'


def fold(lines):
    """{stack: samples} from `perf script` output: a header line per sample, then its frames leaf
    first, then a blank line."""
    stacks = collections.Counter()
    frames = []
    for line in list(lines) + ['']:
        if not line.strip():
            if frames:
                stacks[';'.join(reversed(frames))] += 1
            frames = []
            continue
        if line[0].isspace():
            m = FRAME.match(line.rstrip('\n'))
            if m:
                frames.append(frame_name(*m.groups()))
    return stacks


def read_folded(path):
    stacks = collections.Counter()
    try:
        with open(path) as f:
            for line in f:
                stack, _, count = line.rstrip('\n').rpartition(' ')
                if stack and count.isdigit():
                    stacks[stack] += int(count)
    except OSError:
        pass
    return stacks


def rank(stacks, min_share=0.001):
    """The total sample count, and every function with at least `min_share` of the samples either
    of its own (`self`, as the leaf frame) or with it anywhere on the stack (`total`), most self time
    first. A caller with no time of its own is kept for its total, and the cut stays small, so the
    corpus report can add the projects up before it picks its top functions."""
    own, inclusive = collections.Counter(), collections.Counter()
    for stack, count in stacks.items():
        frames = stack.split(';')
        own[frames[-1]] += count
        for name in set(frames):
            inclusive[name] += count
    samples = sum(stacks.values())
    kept = [name for name in inclusive if max(own[name], inclusive[name]) >= min_share * samples]
    kept.sort(key=lambda name: (-own[name], -inclusive[name], name))
    return dict(samples=samples,
                functions=[dict(function=name, self=own[name], total=inclusive[name]) for name in kept])


def main():
    if sys.argv[1:] != ['fold']:
        sys.exit(__doc__)
    for stack, count in sorted(fold(sys.stdin).items()):
        print(f'{stack} {count}')


if __name__ == '__main__':
    main()
