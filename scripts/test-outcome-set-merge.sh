#!/usr/bin/env bash
# Exercise scripts/outcome-set-merge.sh as a registered git merge driver in a disposable repository.
set -euo pipefail

# Hooks export repository-local settings that override even `git -C`. Never let fixture
# commands inherit the calling repository's object store, worktree, or index.
while IFS= read -r variable; do
    unset "$variable"
done < <(git rev-parse --local-env-vars)

script="$(cd "$(dirname "$0")" && pwd)/outcome-set-merge.sh"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
cd "$fixture"
git init -q -b main
git config user.name fixture
git config user.email fixture@example.invalid
git config commit.gpgsign false
mkdir -p scripts lists/lane
cp "$script" scripts/outcome-set-merge.sh
printf 'lists/** merge=outcome-set\n' >.gitattributes
git config merge.outcome-set.driver \
    'if test -f scripts/outcome-set-merge.sh; then bash scripts/outcome-set-merge.sh %O %A %B; else git merge-file -L ours -L base -L theirs %A %O %B; fi'

failures=0
fail() {
    echo "FAIL: $1" >&2
    failures=$((failures + 1))
}

list=lists/lane/1.0.txt
header='# Files expected to fail.
# The list only shrinks.
'
write() {
    printf '%s' "$header" >"$list"
    printf '%s\n' "$@" >>"$list"
}
commit() {
    git add -A
    git commit -q -m "$1"
}
expect_list() {
    local expected
    expected=$(printf '%s' "$header"; printf '%s\n' "${@:2}")
    if [ "$(cat "$list")" != "${expected%$'\n'}" ]; then
        fail "$1: got"$'\n'"$(cat "$list")"
    fi
}
# Branch `ours` and `theirs` from the base commit, apply each side's list, and merge theirs into ours.
merge_sides() {
    git checkout -q -B ours base
    "$1"
    commit ours
    git checkout -q -B theirs base
    "$2"
    commit theirs
    git checkout -q ours
    git merge -q --no-edit theirs >/dev/null
}

write a.kt b.kt c.kt d.kt e.kt f.kt
commit base
git branch -f base

# Neighbouring removals: a line merge conflicts; the set merge drops both.
remove_b() { write a.kt c.kt d.kt e.kt f.kt; }
remove_c() { write a.kt b.kt d.kt e.kt f.kt; }
if merge_sides remove_b remove_c; then
    expect_list "neighbouring removals" a.kt d.kt e.kt f.kt
else
    fail "neighbouring removals conflicted"
    git merge --abort
fi

# The same entry removed on both sides, beside a removal on one side.
remove_cd() { write a.kt b.kt e.kt f.kt; }
remove_d() { write a.kt b.kt c.kt e.kt f.kt; }
if merge_sides remove_cd remove_d; then
    expect_list "overlapping removals" a.kt b.kt e.kt f.kt
else
    fail "overlapping removals conflicted"
    git merge --abort
fi

# An addition on one side next to a removal on the other keeps the addition and the removal.
add_bb() { write a.kt b.kt bb.kt c.kt d.kt e.kt f.kt; }
if merge_sides add_bb remove_c; then
    expect_list "addition beside removal" a.kt b.kt bb.kt d.kt e.kt f.kt
else
    fail "addition beside removal conflicted"
    git merge --abort
fi

# The driver runs from the top of the worktree even when git is invoked from a subdirectory.
git checkout -q -B ours base
remove_b
commit ours
git checkout -q -B theirs base
remove_c
commit theirs
git checkout -q ours
mkdir -p nested
if (cd nested && git merge -q --no-edit theirs >/dev/null); then
    expect_list "merge from a subdirectory" a.kt d.kt e.kt f.kt
else
    fail "merge from a subdirectory conflicted"
    git merge --abort
fi

# A header changed on one side is taken; the entries still merge as a set.
reheader() {
    local old=$header
    header='# Files expected to fail on this lane.
# The list only shrinks.
'
    write a.kt c.kt d.kt e.kt f.kt
    header=$old
}
if merge_sides reheader remove_c; then
    header='# Files expected to fail on this lane.
# The list only shrinks.
'
    expect_list "header from one side" a.kt d.kt e.kt f.kt
    header='# Files expected to fail.
# The list only shrinks.
'
else
    fail "header from one side conflicted"
    git merge --abort
fi

# Headers changed differently on both sides fall back to the line merge and its conflict.
header_one() { printf '# One.\n' >"$list"; printf '%s\n' a.kt c.kt d.kt e.kt f.kt >>"$list"; }
header_two() { printf '# Two.\n' >"$list"; printf '%s\n' a.kt b.kt d.kt e.kt f.kt >>"$list"; }
if merge_sides header_one header_two 2>/dev/null; then
    fail "conflicting headers merged without a conflict"
else
    grep -q '^<<<<<<< ours$' "$list" || fail "conflicting headers left no conflict markers"
    git merge --abort
fi

# A comment between entries makes the file not a set: the line merge decides.
grouped() { write a.kt '# group' b.kt c.kt d.kt e.kt f.kt; }
if merge_sides grouped remove_c 2>/dev/null; then
    expect_list "grouped file through the line merge" a.kt '# group' b.kt d.kt e.kt f.kt
else
    fail "grouped file conflicted where the line merge succeeds"
    git merge --abort
fi

# Unsorted entries are not a set either; neighbouring edits then conflict as git would report them.
unsorted_base() { write b.kt a.kt c.kt; }
git checkout -q -B unsorted base
unsorted_base
commit unsorted
git branch -f unsorted-base
git checkout -q -B ours unsorted-base
write b.kt c.kt
commit ours
git checkout -q -B theirs unsorted-base
write b.kt a.kt
commit theirs
git checkout -q ours
if git merge -q --no-edit theirs >/dev/null 2>&1; then
    fail "unsorted file merged as a set"
else
    grep -q '^<<<<<<< ours$' "$list" || fail "unsorted file left no conflict markers"
    git merge --abort
fi

# A checkout that predates the driver script merges line by line instead of failing.
git checkout -q -B ours base
git rm -q scripts/outcome-set-merge.sh
remove_b
commit ours
git checkout -q -B theirs base
git rm -q scripts/outcome-set-merge.sh
remove_c
commit theirs
git checkout -q ours
if git merge -q --no-edit theirs >/dev/null 2>&1; then
    fail "a checkout without the driver script merged neighbouring removals"
else
    grep -q '^<<<<<<< ours$' "$list" || fail "a checkout without the driver script left no conflict markers"
    git merge --abort
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures outcome-set merge check(s) failed" >&2
    exit 1
fi
echo "outcome-set merge: all checks passed"
