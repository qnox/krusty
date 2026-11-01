#!/usr/bin/env bash
# Ratchet the legacy surface that the resolution-engine migration deletes.
#
# legacy-baseline.tsv lists one measured quantity per row: <count> TAB <kind> TAB <subject>.
#
#   uses   <identifier>  whole-word occurrences of the identifier in tracked src/ files
#   fns    <prefix>      `fn` definitions in tracked src/ files whose name starts with the prefix
#   lines  <pathspec>    lines of the tracked files the git pathspec matches
#
# Lines starting with `#` and blank lines are kept as they are.
#
#   legacy-ratchet.sh check    fail when a row measures above its baseline count
#   legacy-ratchet.sh tighten  lower each row's count to its current measurement
#   legacy-ratchet.sh measure  print the baseline rows with their current measurements
#
# A count may only go down. `tighten` never raises one: a row that has to grow is edited by hand,
# so the increase is visible in review. Measurements read the working tree of tracked files, so a
# new file counts once it is added to the index.
set -euo pipefail

usage() {
    echo "usage: $0 check|tighten|measure [baseline]" >&2
    exit 2
}

mode=${1:-}
baseline=${2:-legacy-baseline.tsv}
case "$mode" in
check | tighten | measure) ;;
*) usage ;;
esac
[ -f "$baseline" ] || {
    echo "legacy-ratchet: missing $baseline" >&2
    exit 1
}

measure() {
    local kind=$1 subject=$2
    case "$kind" in
    uses)
        { git grep -h -o -w -F -e "$subject" -- src || true; } | wc -l
        ;;
    fns)
        { git grep -h -o -E -e "\\bfn ${subject}[A-Za-z0-9_]*" -- src || true; } | wc -l
        ;;
    lines)
        { git grep -h -c -e '' -- "$subject" || true; } | awk '{ total += $1 } END { print total + 0 }'
        ;;
    *)
        echo "legacy-ratchet: unknown kind '$kind' in $baseline" >&2
        return 1
        ;;
    esac
}

fail=0
output=$(mktemp)
trap 'rm -f "$output"' EXIT
while IFS= read -r line || [ -n "$line" ]; do
    if [ -z "$line" ] || [ "${line#\#}" != "$line" ]; then
        printf '%s\n' "$line" >>"$output"
        continue
    fi
    IFS=$'\t' read -r allowed kind subject <<<"$line"
    if ! [[ "$allowed" =~ ^[0-9]+$ ]] || [ -z "${kind:-}" ] || [ -z "${subject:-}" ]; then
        echo "legacy-ratchet: malformed row in $baseline: $line" >&2
        exit 1
    fi
    current=$(measure "$kind" "$subject" | tr -d ' ')
    case "$mode" in
    check)
        if [ "$current" -gt "$allowed" ]; then
            echo "legacy-ratchet: $kind $subject is $current, above the $allowed allowed." >&2
            fail=1
        elif [ "$current" -lt "$allowed" ]; then
            echo "legacy-ratchet: $kind $subject is $current, below the $allowed allowed; run 'just legacy-baseline' to lock it in."
        fi
        ;;
    tighten)
        if [ "$current" -lt "$allowed" ]; then
            allowed=$current
        fi
        printf '%s\t%s\t%s\n' "$allowed" "$kind" "$subject" >>"$output"
        ;;
    measure)
        printf '%s\t%s\t%s\n' "$current" "$kind" "$subject"
        ;;
    esac
done <"$baseline"

case "$mode" in
check)
    if [ "$fail" -ne 0 ]; then
        cat >&2 <<'EOF'
legacy-ratchet: the legacy surface grew. Route the change through the shared path, or delete the
path it patches, instead of adding to a duplicate (AGENTS.md, "Phase ownership"; docs/RESOLUTION_ENGINE.md).
EOF
        exit 1
    fi
    echo "legacy-ratchet: no row above its baseline"
    ;;
tighten)
    cp "$output" "$baseline"
    echo "legacy-ratchet: tightened $baseline"
    ;;
esac
