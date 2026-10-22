#!/usr/bin/env python3
"""Aggregates the corpus projects' compare.json files into one Markdown table.

usage: summarize.py <compare.json>...
"""
import json
import sys


def main():
    projects = []
    for path in sys.argv[1:]:
        with open(path) as f:
            projects.append(json.load(f))
    projects.sort(key=lambda p: p['name'].lower())
    print('| Project | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes |')
    print('| --- | ---: | ---: | --- | ---: | ---: |')
    built = compilations = identical = classes = 0
    for p in projects:
        measured = p['reference_exit'] == 0
        reference = 'ok' if measured else f"failed (exit {p['reference_exit']})"
        share = f" ({100 * p['identical'] / p['classes']:.1f}%)" if p['classes'] else ''
        print(f"| {p['name']} | {p['loc']['kotlin']:,} | {p['loc']['java']:,} | {reference} | "
              f"{p['built']} / {p['compilations']} | {p['identical']:,} / {p['classes']:,}{share} |")
        if measured:
            built += p['built']
            compilations += p['compilations']
            identical += p['identical']
            classes += p['classes']
    share = f" ({100 * identical / classes:.1f}%)" if classes else ''
    print(f"| **Measured total** | | | | **{built} / {compilations}** | **{identical:,} / {classes:,}{share}** |")


if __name__ == '__main__':
    main()
