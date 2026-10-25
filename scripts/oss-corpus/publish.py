#!/usr/bin/env python3
"""Records one corpus run's results in the results tree the workflow keeps on its results branch.

usage: publish.py <run results dir> <results tree> <projects.tsv> <krusty commit> <date> <run URL>

Each project that produced a comparison in this run replaces its entry under
`<results tree>/projects/<name>/`: its compare.json, with the run that measured it added as
`measured`, and the plugin survey of its reference build. Projects the run did not measure keep
their last result. Entries for projects no longer listed in projects.tsv are removed. One line per
measured project is appended to `<results tree>/history.tsv`.
"""
import glob
import json
import os
import shutil
import sys

HISTORY_HEADER = 'date\tkrusty commit\tproject\treference exit\tbuilt\tcompilations\tidentical\tclasses\trun\n'


def listed(projects_tsv):
    with open(projects_tsv) as f:
        return {line.split('\t')[0] for line in f if line.strip() and not line.startswith('#')}


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
    new = not os.path.exists(history)
    with open(history, 'a') as f:
        if new:
            f.write(HISTORY_HEADER)
        for p in measured:
            f.write(f"{date}\t{commit}\t{p['name']}\t{p['reference_exit']}\t{p['built']}\t"
                    f"{p['compilations']}\t{p['identical']}\t{p['classes']}\t{run}\n")
    return [p['name'] for p in measured]


def main():
    results, tree, projects_tsv, commit, date, run = sys.argv[1:7]
    names = publish(results, tree, listed(projects_tsv), commit, date, run)
    print('published: ' + (' '.join(names) if names else 'no project'))


if __name__ == '__main__':
    main()
