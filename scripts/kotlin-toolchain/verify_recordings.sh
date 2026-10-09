#!/usr/bin/env bash
# Re-record krusty-toolchain's differential corpora from the live references and fail when any
# recording differs from the committed one:
#
# * every project case (`tests/recorded/projects/*.case`) from Kotlin Toolchain `kotlin show modules`,
#   run through the pinned wrapper beside this script;
# * the module-glob corpus (`tests/recorded/globs.tsv`) from the JDK's `glob:` path matcher, through
#   `GlobOracle.java`.
#
# The committed corpora keep local tests fast and offline; this check keeps them true. Run it with
# JAVA_HOME set to a JDK 25. It rewrites the corpora in place, so a failure leaves the difference in
# the working tree to inspect or commit.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
recorded="$root/crates/krusty-toolchain/tests/recorded"
export LC_ALL=C.UTF-8 KOTLIN_CLI_NO_WELCOME_BANNER=1

python3 "$here/record_projects.py" "$here/kotlin" "$recorded"/projects/*.case

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# The oracle's input is each recorded line's pattern and paths: the pattern, then every path with
# its recorded verdict (`+`/`-`) removed. A line whose pattern does not compile records the error
# in place of verdicts and has no paths left to give.
python3 - "$recorded/globs.tsv" "$work/cases.tsv" <<'PY'
import sys
with open(sys.argv[1], encoding="utf-8") as recorded, open(sys.argv[2], "w", encoding="utf-8") as cases:
    for line in recorded.read().split("\n")[:-1]:
        fields = line.split("\t")
        paths = [field[1:] for field in fields[2:] if field[:1] in "+-"]
        cases.write("\t".join([fields[0]] + paths) + "\n")
PY
"$JAVA_HOME/bin/javac" -d "$work" "$here/GlobOracle.java"
"$JAVA_HOME/bin/java" -cp "$work" GlobOracle "$work/cases.tsv" > "$recorded/globs.tsv"

cd "$root"
changed=$(git status --porcelain -- crates/krusty-toolchain/tests/recorded)
if [ -n "$changed" ]; then
    echo "the live references disagree with the committed recordings:" >&2
    echo "$changed" >&2
    git --no-pager diff -- crates/krusty-toolchain/tests/recorded >&2
    exit 1
fi
