#!/usr/bin/env python3
"""Aggregates the corpus projects' results into one Markdown report and one JSON document.

usage: summarize.py [--stars <stars.tsv>] [--plugins <dir>] [--json <out.json>] [--title <text>]
                    <compare.json>...

`--stars` lists `<name>\t<stars>` per project. `--plugins` is searched for the `<name>.plugins.tsv`
plugin surveys the reference builds wrote. The report has the per-project table, the reasons for
every missed compilation, every error krusty's verify compiles reported grouped by cause, what each
build cost, and the Gradle and Kotlin compiler plugins the corpus uses most.
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


# Jars on every Kotlin compile's plugin classpath: the scripting plugin the Kotlin Gradle plugin
# always adds, and its dependencies. They say nothing about what a project chose.
DEFAULT_COMPILER_JARS = re.compile(
    r'^(kotlin-stdlib|annotations|kotlin-script-runtime|kotlin-reflect|trove4j|kotlin-scripting-.*)$')


def chosen(kind, plugin):
    """Whether a surveyed plugin is one the project applies, not Gradle core, a plugin class
    without an id, or a default compiler classpath jar."""
    if kind == 'gradle':
        return not plugin.startswith('org.gradle.') and not plugin.rsplit('.', 1)[-1][:1].isupper()
    if kind == 'compiler':
        return not DEFAULT_COMPILER_JARS.match(plugin)
    return True


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
                    plugin = plugin_name(kind, value)
                    if chosen(kind, plugin):
                        kinds[kind][plugin].add(module)
        surveys[name] = {kind: {p: len(m) for p, m in plugins.items()} for kind, plugins in kinds.items()}
    return surveys


def fallback_reason(reason):
    """A fallback reason reduced to what krusty refused: `compiler plugin <id>` and
    `argument <flag>` items, or the reason itself when it names neither."""
    items = []
    # The plugin is named twice, by id and by jar; the jar is the fallback for a plugin without an id.
    plugins = (re.findall(r"unsupported compiler plugin 'plugin id '([^']+)''", reason)
               or re.findall(r"unsupported compiler plugin '[^']*/([^/']+?)(?:-\d[\w.+-]*)?\.jar'", reason))
    for plugin in plugins:
        item = 'compiler plugin ' + plugin
        if item not in items:
            items.append(item)
    for argument in re.findall(r"kotlinc argument '([^']+)'", reason):
        item = 'argument ' + argument
        if item not in items:
            items.append(item)
    return ', '.join(items) if items else reason


def misses(project):
    """(reason, compilations) for every compilation krusty did not build, most frequent first."""
    reasons = collections.Counter()
    for row in project['rows']:
        if not row['built']:
            reason = (f"left to kotlinc: {fallback_reason(row['fallback'])}" if row.get('fallback')
                      else row.get('error') or 'krusty wrote no class')
            reasons[reason] += 1
    return reasons.most_common()


QUOTED = {quote: re.compile(f'{quote}[^{quote}]*{quote}') for quote in ("'", '`', '"')}


def cause(error):
    """An error reduced to its cause: its message without the location before `error: `, with
    quoted text and numbers masked. `A.kt:3:5: error: unresolved reference 'bar'.` becomes
    `unresolved reference '…'.`. A quote character that does not pair up is an apostrophe and stays."""
    message = error.split(': error: ', 1)[1] if ': error: ' in error else error.removeprefix('error: ')
    for quote, spans in QUOTED.items():
        if message.count(quote) % 2 == 0:
            message = spans.sub(f'{quote}…{quote}', message)
    return re.sub(r'\b\d+\b', 'N', message.strip())


def error_causes(project):
    """Every distinct error krusty's verify compiles of a project reported, grouped by cause:
    [(cause, [(error, [compilation task paths])])], the cause seen in the most compilations first."""
    found = collections.defaultdict(lambda: collections.defaultdict(list))
    for row in project['rows']:
        for error in dict.fromkeys((row.get('verify') or {}).get('errors', [])):
            found[cause(error)][error].append(row['task'])
    ranked = [(c, sorted(errors.items())) for c, errors in found.items()]

    def compilations(entry):
        return len({task for _, tasks in entry[1] for task in tasks})
    return sorted(ranked, key=lambda entry: (-compilations(entry), -len(entry[1]), entry[0]))


def counted(number, noun):
    return f'{number} {noun}' + ('' if number == 1 else 's')


def code(text):
    """`text` as inline code, fenced by more backticks than it holds in a row."""
    fence = '`' * (max((len(run) for run in re.findall('`+', text)), default=0) + 1)
    padding = ' ' if text.startswith('`') or text.endswith('`') else ''
    return f'{fence}{padding}{text}{padding}{fence}'


def verification(project):
    """How far the verify build of a project got, or None for a result measured before the corpus
    ran one (it has no `verify_exit`): the build's exit code, and how many compilations have a
    verify record and how many have none (no verify compile, or one that wrote no record)."""
    if project.get('verify_exit') is None:
        return None
    verified = sum(1 for r in project['rows']
                   if r.get('verify') and r['verify']['status'] != 'no record')
    return dict(exit=project['verify_exit'], verified=verified, unverified=len(project['rows']) - verified)


def errors_section(projects):
    """Every error krusty reported when it compiled each compilation on its own, grouped by cause:
    one table across the corpus, then, for every project the verify build ran for, its errors under
    their cause with the compilations that reported them, or that it verified nothing."""
    measured = [p for p in projects if verification(p) is not None]
    if not measured:
        return []
    out = ['', '## krusty errors', '',
           'The verify build compiles every compilation with kotlinc, and krusty compiles each one on its own',
           'beside it, against kotlinc\'s classes of the compilations it depends on (the krusty plugin\'s',
           'verify mode). One failure therefore never hides the next: these are all the errors krusty',
           'reported, grouped by cause, the message without its location and with quoted text and numbers',
           'masked. A result measured before the corpus ran the verify build is left out.', '']
    causes = {p['name']: error_causes(p) for p in measured}
    corpus = collections.defaultdict(lambda: [set(), set(), set()])
    for name, ranked in causes.items():
        for c, errors in ranked:
            corpus[c][0].add(name)
            corpus[c][1].update((name, task) for _, tasks in errors for task in tasks)
            corpus[c][2].update((name, error) for error, _ in errors)
    if corpus:
        out += ['| Cause | Projects | Compilations | Errors |', '| --- | ---: | ---: | ---: |']
        for c, (names, tasks, errors) in sorted(
                corpus.items(), key=lambda kv: (-len(kv[1][0]), -len(kv[1][1]), -len(kv[1][2]), kv[0])):
            cell = code(c).replace('|', '\\|')
            out.append(f'| {cell} | {len(names)} | {len(tasks)} | {len(errors)} |')
    else:
        out.append('No verified compilation reported an error.')
    for p in measured:
        ranked = causes[p['name']]
        state = verification(p)
        if not state['verified']:
            out += ['', f"- **{p['name']}**: no compilation verified (verify build exit {state['exit']})"]
            continue
        failed = {task for _, errors in ranked for _, tasks in errors for task in tasks}
        summary = (f"{counted(sum(len(errors) for _, errors in ranked), 'error')} in "
                   f"{counted(len(failed), 'compilation')}, {counted(len(ranked), 'cause')}; "
                   f"{state['verified']} of {counted(len(p['rows']), 'compilation')} verified")
        if state['exit']:
            summary += f" (verify build exit {state['exit']})"
        out += ['', f"<details><summary><b>{p['name']}</b>: {summary}</summary>", '']
        if not ranked:
            out.append('None.')
        for c, errors in ranked:
            out.append(f'- {code(c)}')
            for error, tasks in errors:
                out.append(f"  - {code(error)} in {', '.join(code(task) for task in tasks)}")
        out += ['', '</details>']
    return out


def share(part, whole):
    return f' ({100 * part / whole:.1f}%)' if whole else ''


def measured_cell(project):
    """`2026-10-25, krusty 4dd7475` linked to the run, from the `measured` record publishing adds."""
    m = project.get('measured')
    if not m:
        return ''
    text = f"{m['date']}, krusty {m['commit'][:7]}"
    return f"[{text}]({m['run']})" if m.get('run') else text


def seconds(value):
    return f'{value:,.1f} s'


def pair(kotlinc, krusty, unit, speedup=False):
    """`kotlinc / krusty`, and with `speedup` how many times faster krusty is."""
    text = f'{unit(kotlinc)} / {unit(krusty)}'
    if speedup and kotlinc and krusty:
        text += f' ({kotlinc / krusty:.1f}×)'
    return text


def megabytes(value):
    return f'{value:,} MB' if value else '—'


def cost_section(projects):
    """The cost of both builds of every project measured with them: compile time, the compiler's and
    the whole build's peak memory, and the whole build's wall and CPU time."""
    measured = [p for p in projects if p.get('cost') and p['cost'].get('kotlinc') and p['cost'].get('krusty')]
    if not measured:
        return []
    out = ['', '## Build cost', '',
           'Each build runs alone on its own runner. Compile time sums the compile tasks: kotlinc\'s Kotlin task, and',
           'krusty\'s replacement, support check and Kotlin task (kotlinc\'s compile when a compilation is left to it).',
           'Peak memory sums every process the build started, daemons included; the compiler\'s is the Kotlin daemon\'s',
           'in the reference build and the krusty processes\' in the krusty build. A warm-up build downloads everything',
           'first, and both measured builds run offline from the same caches.',
           '',
           '| Project | Compile time, compilations krusty built (kotlinc / krusty) | Compile time, all compilations |'
           ' Compiler peak RSS | Build peak RSS | Build wall time | Build CPU time |',
           '| --- | ---: | ---: | ---: | ---: | ---: | ---: |']
    totals = collections.Counter()
    for p in measured:
        k, r = p['cost']['kotlinc'], p['cost']['krusty']
        out.append(
            f"| {p['name']} | {pair(k['built_compile_seconds'], r['built_compile_seconds'], seconds, True)} | "
            f"{pair(k['compile_seconds'], r['compile_seconds'], seconds)} | "
            f"{pair(k['peak_rss_mb']['kotlin_daemon'], r['peak_rss_mb']['krusty'], megabytes)} | "
            f"{pair(k['peak_rss_mb']['total'], r['peak_rss_mb']['total'], megabytes)} | "
            f"{pair(k['wall_seconds'], r['wall_seconds'], seconds)} | "
            f"{pair(k['cpu_seconds'], r['cpu_seconds'], seconds)} |")
        for key in ('built_compile_seconds', 'compile_seconds', 'wall_seconds', 'cpu_seconds'):
            totals['kotlinc', key] += k[key]
            totals['krusty', key] += r[key]
    total = {key: pair(totals['kotlinc', key], totals['krusty', key], seconds, key == 'built_compile_seconds')
             for key in ('built_compile_seconds', 'compile_seconds', 'wall_seconds', 'cpu_seconds')}
    out.append(f"| **Total** | **{total['built_compile_seconds']}** | **{total['compile_seconds']}** | | | "
               f"**{total['wall_seconds']}** | **{total['cpu_seconds']}** |")
    return out


def hotspot_section(projects):
    """The functions the sampled krusty builds spend the most CPU time in across the corpus, as a
    share of every krusty sample, and each project's flame graph."""
    sampled = [p for p in projects if p.get('hotspots') and p['hotspots']['samples']]
    if not sampled:
        return []
    samples = sum(p['hotspots']['samples'] for p in sampled)
    own, total, where = collections.Counter(), collections.Counter(), collections.defaultdict(set)
    for p in sampled:
        for f in p['hotspots']['functions']:
            own[f['function']] += f['self']
            total[f['function']] += f['total']
            where[f['function']].add(p['name'])
    out = ['', '## krusty hotspots', '',
           f'Sampled call stacks of the krusty processes of every krusty build ({samples:,} samples at 99 Hz).',
           '*Self* counts the samples in the function itself, *total* those with it anywhere on the stack.', '']
    graphs = [p['name'] for p in sampled if p.get('flame_graph')]
    if graphs:
        out += ['Flame graphs: ' + ', '.join(
            f"[{name}](projects/{name}/krusty-flamegraph.svg)" for name in graphs) + '.', '']
    out += ['| Function | Self | Total | Projects |', '| --- | ---: | ---: | --- |']
    for name, count in sorted(own.items(), key=lambda kv: (-kv[1], kv[0]))[:20]:
        cell = name.replace('|', '\\|')
        out.append(f"| `{cell}` | {100 * count / samples:.1f}% | "
                   f"{100 * total[name] / samples:.1f}% | {', '.join(sorted(where[name]))} |")
    return out


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

    out += errors_section(projects)

    out += cost_section(projects)
    out += hotspot_section(projects)

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
                                          errors=[dict(cause=c, errors=[dict(error=e, compilations=t) for e, t in errors])
                                                  for c, errors in error_causes(p)],
                                          verification=verification(p),
                                          plugins=surveys.get(p['name'], {})) for p in projects]),
                      f, indent=1)


if __name__ == '__main__':
    main()
