#!/usr/bin/env bash
# Regenerates expected.kt for every formatting fixture by running the official ktlint CLI.
# Usage: crates/krusty-lsp/tools/bless-formatting.sh [case-name ...]
# KTLINT_BIN overrides the ktlint executable (default: tools/ktlint/ktlint at repo root).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../../.." && pwd)"
ktlint_bin="${KTLINT_BIN:-$repo_root/tools/ktlint/ktlint}"
fixtures="$repo_root/crates/krusty-lsp/tests/fixtures/formatting"

if [[ ! -x "$ktlint_bin" ]]; then
    echo "ktlint binary not found at $ktlint_bin" >&2
    exit 1
fi

bless_case() {
    local case_dir="$1"
    local name
    name="$(basename "$case_dir")"
    local work
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' RETURN
    cp "$case_dir/input.kt" "$work/Input.kt"
    if [[ -f "$case_dir/.editorconfig" ]]; then
        cp "$case_dir/.editorconfig" "$work/.editorconfig"
    fi
    # ktlint exit codes: 0 = clean, 1 = lint violations remained after autocorrect
    # (e.g. standard:filename). Anything else means ktlint itself failed, in which case
    # Input.kt may still hold the unformatted original and must not be blessed.
    local status=0
    (cd "$work" && "$ktlint_bin" --format "Input.kt" >/dev/null 2>&1) || status=$?
    if [[ $status -gt 1 || ! -f "$work/Input.kt" ]]; then
        echo "ktlint failed for $name (exit $status)" >&2
        rm -rf "$work"
        exit 1
    fi
    cp "$work/Input.kt" "$case_dir/expected.kt"
    echo "blessed $name"
}

if [[ $# -gt 0 ]]; then
    for name in "$@"; do
        case "$name" in
            */* | *..*)
                echo "invalid case name: $name" >&2
                exit 1
                ;;
        esac
        if [[ -d "$fixtures/$name" ]]; then
            bless_case "$fixtures/$name"
        else
            bless_case "$fixtures/../formatting-todo/$name"
        fi
    done
else
    for case_dir in "$fixtures"/*/; do
        bless_case "$case_dir"
    done
fi
