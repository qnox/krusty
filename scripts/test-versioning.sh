#!/usr/bin/env bash
# Exercise the real version recipes against isolated, disposable Git history.
set -euo pipefail

# Hooks export repository-local settings that override even `git -C`. Never let fixture
# commands inherit the calling repository's object store, worktree, or index.
while IFS= read -r variable; do
    unset "$variable"
done < <(git rev-parse --local-env-vars)

repo=$(cd "$(dirname "$0")/.." && pwd)
reference=$(cd "$repo" && just max-version)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
cp "$repo/justfile" "$repo/push.just" "$fixture/"

export GIT_AUTHOR_NAME='Anton Efimchuk'
export GIT_AUTHOR_EMAIL='anton.efimchuk@gmail.com'
export GIT_COMMITTER_NAME='Anton Efimchuk'
export GIT_COMMITTER_EMAIL='anton.efimchuk@gmail.com'
git -C "$fixture" init -q
git -C "$fixture" commit --allow-empty -qm 'Version fixture baseline'
baseline=$(git -C "$fixture" rev-parse HEAD)
git -C "$fixture" commit --allow-empty -qm 'Version fixture first descendant'
git -C "$fixture" commit --allow-empty -qm 'Version fixture second descendant'

failures=0
expect_output() {
    local expected=$1
    shift
    local actual
    if ! actual=$(cd "$fixture" && just "$@"); then
        echo "FAIL: $* unexpectedly failed" >&2
        failures=$((failures + 1))
    elif [[ $actual != "$expected" ]]; then
        echo "FAIL: $*: expected '$expected', got '$actual'" >&2
        failures=$((failures + 1))
    fi
}

expect_rejected() {
    local actual
    if actual=$(cd "$fixture" && just "$@" 2>"$fixture/rejected.stderr"); then
        echo "FAIL: $* accepted an invalid baseline: '$actual'" >&2
        failures=$((failures + 1))
    elif [[ -n $actual ]]; then
        echo "FAIL: $* emitted a version despite failure: '$actual'" >&2
        failures=$((failures + 1))
    fi
}

printf '%s %s\n' "$reference" "$baseline" >"$fixture/kotlin-versions"
expect_output 3 build-number "$reference"
expect_output "$reference-build.3" version

printf '%s -\n' "$reference" >"$fixture/kotlin-versions"
expect_output 3 build-number "$reference"
expect_output "$reference-build.3" version

printf '%s refs/heads/missing-version-baseline\n' "$reference" >"$fixture/kotlin-versions"
expect_rejected build-number "$reference"
expect_rejected version

# An old release tag can keep an unrelated baseline available after a history rewrite.
unrelated=$(git -C "$fixture" commit-tree "$baseline^{tree}" -m 'Unrelated version baseline')
git -C "$fixture" tag unrelated-version-baseline "$unrelated"
printf '%s refs/tags/unrelated-version-baseline\n' "$reference" >"$fixture/kotlin-versions"
expect_rejected build-number "$reference"
expect_rejected version

if [[ $failures -ne 0 ]]; then
    echo "versioning: $failures failed assertions" >&2
    exit 1
fi
echo 'versioning: 8 assertions passed'
