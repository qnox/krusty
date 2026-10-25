#!/usr/bin/env python3
"""Aggregates the corpus projects' results into one Markdown report and one JSON document.

usage: summarize.py [--stars <stars.tsv>] [--plugins <dir>] [--json <out.json>] [--title <text>]
                    <compare.json>...

`--stars` lists `<name>\t<stars>` per project. `--plugins` is searched for the `<name>.plugins.tsv`
plugin surveys the reference builds wrote. The report has the per-project table, the reasons for
every missed compilation, and the Gradle and Kotlin compiler plugins the corpus uses most.
"""
import argparse
import collections
import glob
import json
import os
import re


def read_stars(path):
    stars = {}
    if path:
        with open(path) as f:
            for line in f:
                fields = line.rstrip('\n').split('\t')
                if len(fields) == 2 and fields[1].isdigit():
                    stars[fields[0]] = int(fields[1])
    return stars


def plugin_name(kind, value):
    """A compiler plugin jar without its version (`kotlin-compose-compiler-plugin-embeddable`)."""
    if kind == 'compiler':
        return re.sub(r'-\d[\w.+-]*\.jar$', '', value).removesuffix('.jar')
    return value


def read_plugins(directory, names):
    """{project: {kind: {plugin: modules using it}}} from each project's plugin survey."""
    surveys = {}
    if not directory:
        return surveys
    for name in names:
        found = glob.glob(os.path.join(directory, '**', f'{name}.plugins.tsv'), recursive=True)
        if not found:
            continue
        kinds = collections.defaultdict(lambda: collections.defaultdict(set))
        with open(found[0]) as f:
            for line in f:
                fields = line.rstrip('\n').split('\t')
                if len(fields) == 3:
                    kind, module, value = fields
                    kinds[kind][plugin_name(kind, value)].add(module)
        surveys[name] = {kind: {p: len(m) for p, m in plugins.items()} for kind, plugins in kinds.items()}
    return surveys


def misses(project):
    """(reason, compilations) for every compilation krusty did not build, most frequent first."""
    reasons = collections.Counter()
    for row in project['rows']:
        if not row['built']:
            reason = (f"left to kotlinc: {row['fallback']}" if row.get('fallback')
                      else row.get('error') or 'krusty wrote no class')
            reasons[reason] += 1
    return reasons.most_common()


def share(part, whole):
    return f' ({100 * part / whole:.1f}%)' if whole else ''


def measured_cell(project):
    """`2026-10-25, krusty 4dd7475` linked to the run, from the `measured` record publishing adds."""
    m = project.get('measured')
    if not m:
        return ''
    text = f"{m['date']}, krusty {m['commit'][:7]}"
    return f"[{text}]({m['run']})" if m.get('run') else text


def report(projects, stars, surveys, title):
    out = []
    if title:
        out += [f'# {title}', '']
    # A published result carries the run that measured it: projects can come from different runs.
    dated = any('measured' in p for p in projects)
    out += ['| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes |'
            + (' Measured |' if dated else ''),
            '| --- | ---: | ---: | ---: | --- | ---: | ---: |' + (' --- |' if dated else '')]
    built = compilations = identical = classes = 0
    for p in projects:
        measured = p['reference_exit'] == 0
        reference = 'ok' if measured else f"failed (exit {p['reference_exit']})"
        star = f"{stars[p['name']]:,}" if p['name'] in stars else ''
        out.append(f"| {p['name']} | {star} | {p['loc']['kotlin']:,} | {p['loc']['java']:,} | {reference} | "
                   f"{p['built']} / {p['compilations']} | {p['identical']:,} / {p['classes']:,}"
                   f"{share(p['identical'], p['classes'])} |" + (f" {measured_cell(p)} |" if dated else ''))
        if measured:
            built += p['built']
            compilations += p['compilations']
            identical += p['identical']
            classes += p['classes']
    out.append(f'| **Measured total** | | | | | **{built} / {compilations}** | '
               f'**{identical:,} / {classes:,}{share(identical, classes)}** |' + (' |' if dated else ''))

    out += ['', '## Missed compilations', '']
    any_miss = False
    for p in projects:
        reasons = misses(p)
        if not reasons:
            continue
        any_miss = True
        out.append(f"- **{p['name']}**")
        for reason, count in reasons:
            out.append(f'  - {count} × {reason}')
    if not any_miss:
        out.append('None.')

    for kind, heading in (('gradle', 'Gradle plugins'), ('compiler', 'Kotlin compiler plugins'),
                          ('compiler-arg', 'Compiler plugin arguments')):
        used = collections.Counter()
        modules = collections.Counter()
        for survey in surveys.values():
            for plugin, count in survey.get(kind, {}).items():
                used[plugin] += 1
                modules[plugin] += count
        if not used:
            continue
        out += ['', f'## {heading}', '', '| Plugin | Projects | Modules |', '| --- | ---: | ---: |']
        for plugin, count in sorted(used.items(), key=lambda kv: (-kv[1], -modules[kv[0]], kv[0]))[:30]:
            out.append(f'| `{plugin}` | {count} | {modules[plugin]} |')
    return '\n'.join(out) + '\n'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--stars')
    parser.add_argument('--plugins')
    parser.add_argument('--json')
    parser.add_argument('--title')
    parser.add_argument('reports', nargs='+')
    args = parser.parse_args()
    projects = []
    for path in args.reports:
        with open(path) as f:
            projects.append(json.load(f))
    projects.sort(key=lambda p: p['name'].lower())
    stars = read_stars(args.stars)
    surveys = read_plugins(args.plugins, [p['name'] for p in projects])
    print(report(projects, stars, surveys, args.title), end='')
    if args.json:
        with open(args.json, 'w') as f:
            json.dump(dict(projects=[dict(p, stars=stars.get(p['name']), misses=misses(p),
                                          plugins=surveys.get(p['name'], {})) for p in projects]),
                      f, indent=1)


if __name__ == '__main__':
    main()
