#!/usr/bin/env python3
"""Records one corpus run's results in the results tree the workflow keeps on its results branch.

usage: publish.py <run results dir> <results tree> <projects.tsv> <krusty commit> <date> <run URL>

Each project that produced a comparison in this run replaces its entry under
`<results tree>/projects/<name>/`: its compare.json, with the run that measured it added as
`measured`, and the plugin survey of its reference build. Projects the run did not measure keep
their last result. Entries for projects no longer listed in projects.tsv are removed. One line per
measured project is appended to `<results tree>/history.tsv`, with what its two builds cost: the
compile time of the compilations krusty built, peak memory and wall time, kotlinc's then krusty's.
Lines from runs that did not measure cost leave those columns out.
"""
import glob
import json
import os
import shutil
import sys

HISTORY_HEADER = ('date\tkrusty commit\tproject\treference exit\tbuilt\tcompilations\tidentical\tclasses\trun\t'
                  'kotlinc built compile s\tkrusty built compile s\tkotlinc peak MB\tkrusty peak MB\t'
                  'kotlinc wall s\tkrusty wall s\n')


def listed(projects_tsv):
    with open(projects_tsv) as f:
        return {line.split('\t')[0] for line in f if line.strip() and not line.startswith('#')}


def cost_columns(project):
    cost = project.get('cost') or {}
    kotlinc, krusty = cost.get('kotlinc'), cost.get('krusty')
    if not kotlinc or not krusty:
        return ''
    return '\t' + '\t'.join(str(v) for v in (
        kotlinc['built_compile_seconds'], krusty['built_compile_seconds'],
        kotlinc['peak_rss_mb']['total'], krusty['peak_rss_mb']['total'],
        kotlinc['wall_seconds'], krusty['wall_seconds']))


def publish(results, tree, names, commit, date, run):
    projects_dir = os.path.join(tree, 'projects')
    os.makedirs(projects_dir, exist_ok=True)
    measured = []
    for path in sorted(glob.glob(os.path.join(results, '**', 'compare.json'), recursive=True)):
        with open(path) as f:
            project = json.load(f)
        name = project['name']
        if name not in names:
            continue
        project['measured'] = dict(date=date, commit=commit, run=run)
        target = os.path.join(projects_dir, name)
        shutil.rmtree(target, ignore_errors=True)
        os.makedirs(target)
        with open(os.path.join(target, 'compare.json'), 'w') as f:
            json.dump(project, f, indent=1)
        # The artifact holds out/<name>/compare.json beside logs/<name>.plugins.tsv.
        survey = os.path.join(os.path.dirname(path), '..', '..', 'logs', f'{name}.plugins.tsv')
        if os.path.isfile(survey):
            shutil.copy(survey, os.path.join(target, f'{name}.plugins.tsv'))
        measured.append(project)
    for entry in os.listdir(projects_dir):
        if entry not in names:
            shutil.rmtree(os.path.join(projects_dir, entry))
    history = os.path.join(tree, 'history.tsv')
    lines = [HISTORY_HEADER]
    if os.path.exists(history):
        with open(history) as f:
            # The header names every column, those added since earlier lines were written too.
            lines += f.readlines()[1:]
    for p in measured:
        lines.append(f"{date}\t{commit}\t{p['name']}\t{p['reference_exit']}\t{p['built']}\t"
                     f"{p['compilations']}\t{p['identical']}\t{p['classes']}\t{run}{cost_columns(p)}\n")
    with open(history, 'w') as f:
        f.writelines(lines)
    return [p['name'] for p in measured]


def main():
    results, tree, projects_tsv, commit, date, run = sys.argv[1:7]
    names = publish(results, tree, listed(projects_tsv), commit, date, run)
    print('published: ' + (' '.join(names) if names else 'no project'))


if __name__ == '__main__':
    main()
