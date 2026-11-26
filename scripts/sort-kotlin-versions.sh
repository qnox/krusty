#!/usr/bin/env bash
# Sort Kotlin release names, oldest first, on stdin to stdout. `sort -V` alone puts `2.5.0` before
# `2.5.0-RC`; a release follows its own pre-releases (`2.5.0-Beta1`, `2.5.0-RC`, `2.5.0-RC2`, `2.5.0`).
# The suffix is kept as written: `~` stands in for its `-` only while sorting, where it ranks below
# the end of the name.
set -euo pipefail

sed 's/-/~/' | sort -V | sed 's/~/-/'
