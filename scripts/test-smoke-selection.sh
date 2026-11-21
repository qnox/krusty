#!/usr/bin/env bash
# Exercise scripts/smoke.sh's lane selection against a disposable repository whose branch deletes,
# renames and edits files. The selection is printed before any check or build runs.
set -euo pipefail

# Hooks export repository-local settings that override even `git -C`. Never let fixture
# commands inherit the calling repository's object store, worktree, or index.
while IFS= read -r variable; do
    unset "$variable"
done < <(git rev-parse --local-env-vars)

source_scripts="$(cd "$(dirname "$0")" && pwd)"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
cd "$fixture"
git init -q -b master
git config user.name fixture
git config user.email fixture@example.invalid
mkdir -p scripts tests/box_expected_failures/wasm-js-klib tests/box_expected_failures/native-klib \
    tests/box_expected_failures/jvm crates/krusty-wasm/src docs
cp "$source_scripts/smoke.sh" "$source_scripts/test-gate-defaults.sh" "$source_scripts/test-deadline.sh" scripts/
echo a >tests/box_expected_failures/wasm-js-klib/2.4.21.txt
echo b >tests/box_expected_failures/native-klib/2.4.21.txt
echo c >tests/box_expected_failures/jvm/2.4.21.txt
echo d >crates/krusty-wasm/src/lib.rs
echo e >docs/guide.md
git add -A
git commit -q -m base

failures=0
fail() {
    echo "FAIL: $*" >&2
    failures=$((failures + 1))
}

# Run the selection for the branch's commits since master.
selection() {
    KRUSTY_SMOKE_PRINT_LANES=1 bash scripts/smoke.sh master
}

expect() {
    local scenario=$1 expected=$2 actual
    actual=$(selection)
    [ "$actual" = "$expected" ] || fail "$scenario: expected '$expected', got '$actual'"
}

reset_branch() {
    git checkout -q master
    git branch -q -D change 2>/dev/null || true
    git checkout -q -b change
}

reset_branch
echo more >>docs/guide.md
git commit -q -am docs
expect "a documentation edit selects no lane" ""

reset_branch
git rm -q tests/box_expected_failures/wasm-js-klib/2.4.21.txt
git commit -q -m "delete an expectation list"
expect "a deleted Wasm expectation list selects both Wasm lanes" "wasm-js wasm-wasi"

reset_branch
git mv tests/box_expected_failures/native-klib/2.4.21.txt docs/native-moved.md
git commit -q -m "rename a Native list out of its lane"
expect "a rename out of a Native-owned path selects Native" "native"

reset_branch
git mv docs/guide.md tests/box_expected_failures/jvm/guide.txt
git commit -q -m "rename into a JVM-owned path"
expect "a rename into a JVM-owned path selects JVM" "jvm"

reset_branch
echo conformance >scripts/conformance-regressions.sh
git add scripts/conformance-regressions.sh
git commit -q -m "add a conformance script"
expect "a conformance script selects every lane" "jvm native wasm-js wasm-wasi"

reset_branch
git rm -q crates/krusty-wasm/src/lib.rs
git commit -q -m "delete a Wasm source"
expect "a deleted Wasm crate source selects both Wasm lanes" "wasm-js wasm-wasi"

reset_branch
echo x >>tests/box_expected_failures/native-klib/2.4.21.txt
git commit -q -am "edit a Native list"
git checkout -q master
git checkout -q -b change-merge change
git merge -q --no-ff -m "merge master" master 2>/dev/null || true
expect "a merge of the base adds nothing beyond the edit" "native"

[ "$failures" -eq 0 ] || exit 1
echo "smoke selection tests passed"
