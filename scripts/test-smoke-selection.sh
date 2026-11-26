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
    tests/box_expected_failures/jvm crates/krusty-wasm/src docs src/plugins/compose src/plugins/parcelize \
    tests/grouped_e2e
cp "$source_scripts/smoke.sh" "$source_scripts/test-gate-defaults.sh" "$source_scripts/test-deadline.sh" scripts/
echo a >tests/box_expected_failures/wasm-js-klib/2.4.21.txt
echo b >tests/box_expected_failures/native-klib/2.4.21.txt
echo c >tests/box_expected_failures/jvm/2.4.21.txt
echo d >crates/krusty-wasm/src/lib.rs
echo e >docs/guide.md
echo 'fn a() {}' >src/plugins/compose/lower.rs
echo 'fn b() {}' >src/plugins/parcelize/lower.rs
echo 'fn c() {}' >src/plugins/registry.rs
echo '// pins a Compose diagnostic' >tests/cli_plugins_e2e.rs
echo '// names no plugin' >tests/unrelated_e2e.rs
echo '// composes a Compose module' >tests/grouped_e2e/compose_case.rs
echo '// Parcelize' >tests/parcelize_shapes_e2e.rs
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

# The e2e modules a change selects, one per line, before any check or build runs.
e2e_selection() {
    KRUSTY_SMOKE_PRINT_E2E=1 bash scripts/smoke.sh master
}

expect_e2e() {
    local scenario=$1 expected=$2 actual
    actual=$(e2e_selection)
    [ "$actual" = "$expected" ] || fail "$scenario: expected '$expected', got '$actual'"
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

reset_branch
echo more >>src/plugins/compose/lower.rs
git commit -q -am "edit a plugin source"
expect_e2e "a plugin source selects the e2e modules that name the plugin, files and directories alike" \
    "cli_plugins_e2e::
grouped_e2e::"

reset_branch
echo more >>src/plugins/parcelize/lower.rs
git commit -q -am "edit another plugin source"
expect_e2e "the plugin is taken from the changed path, not from a list" "parcelize_shapes_e2e::"

reset_branch
echo more >>src/plugins/registry.rs
git commit -q -am "edit a shared plugin source"
expect_e2e "a source file directly under plugins names no plugin" ""

reset_branch
echo more >>src/plugins/compose/lower.rs
echo '// pins a Compose diagnostic, edited' >>tests/cli_plugins_e2e.rs
git commit -q -am "edit a plugin source and a covering module"
expect_e2e "a module that is both changed and covering is selected once" "cli_plugins_e2e::
grouped_e2e::"

[ "$failures" -eq 0 ] || exit 1
echo "smoke selection tests passed"
