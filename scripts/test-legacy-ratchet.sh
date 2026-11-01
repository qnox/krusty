#!/usr/bin/env bash
# Exercise scripts/legacy-ratchet.sh against a disposable repository.
set -euo pipefail

# Hooks export repository-local settings that override even `git -C`. Never let fixture
# commands inherit the calling repository's object store, worktree, or index.
while IFS= read -r variable; do
    unset "$variable"
done < <(git rev-parse --local-env-vars)

script="$(cd "$(dirname "$0")" && pwd)/legacy-ratchet.sh"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
cd "$fixture"
git init -q
mkdir -p src/bridge
cat >src/lib.rs <<'EOF'
fn unify_ty() {}
fn unify_ty_from_symbols() { unify_ty(); }
fn select_call() {}
fn select_property() {}
fn reselect() {}
EOF
printf 'a\nb\n' >src/bridge.rs
printf 'c\nd\ne\n' >src/bridge/part.rs
printf 'not tracked\n' >src/bridge/untracked.rs
git add src/lib.rs src/bridge.rs src/bridge/part.rs
printf '# legacy surface\n2\tuses\tunify_ty\n2\tfns\tselect_\n5\tlines\tsrc/bridge*\n' >baseline.tsv

failures=0
fail() {
    echo "FAIL: $*" >&2
    failures=$((failures + 1))
}

expected_measure=$'2\tuses\tunify_ty\n2\tfns\tselect_\n5\tlines\tsrc/bridge*'
actual=$("$script" measure baseline.tsv)
[ "$actual" = "$expected_measure" ] || fail "measure: expected '$expected_measure', got '$actual'"

"$script" check baseline.tsv >/dev/null || fail "check rejected a tree at its baseline"

# A new use of a ratcheted identifier fails the check; a longer identifier is not a use.
echo 'fn caller() { unify_ty(); unify_ty_from_symbols(); }' >>src/lib.rs
if errors=$("$script" check baseline.tsv 2>&1 >/dev/null); then
    fail "check accepted a new use"
elif [ "$(printf '%s\n' "$errors" | head -1)" != "legacy-ratchet: uses unify_ty is 3, above the 2 allowed." ]; then
    fail "check reported '$errors'"
fi
git checkout -q -- src/lib.rs

# A new tracked file under the ratcheted path counts once it is in the index.
git add src/bridge/untracked.rs
"$script" check baseline.tsv >/dev/null 2>&1 && fail "check accepted a grown bridge"
git rm -q --cached src/bridge/untracked.rs

# Deleting surface passes, and tighten lowers the row without touching the others.
printf 'fn unify_ty() {}\nfn select_call() {}\nfn select_property() {}\n' >src/lib.rs
"$script" check baseline.tsv >/dev/null || fail "check rejected a smaller surface"
"$script" tighten baseline.tsv >/dev/null
expected_baseline=$'# legacy surface\n1\tuses\tunify_ty\n2\tfns\tselect_\n5\tlines\tsrc/bridge*'
[ "$(cat baseline.tsv)" = "$expected_baseline" ] || fail "tighten wrote '$(cat baseline.tsv)'"

# Tighten never raises a row.
printf 'fn unify_ty() { unify_ty(); unify_ty(); }\nfn select_call() {}\nfn select_property() {}\n' >src/lib.rs
"$script" tighten baseline.tsv >/dev/null
[ "$(cat baseline.tsv)" = "$expected_baseline" ] || fail "tighten raised a row: '$(cat baseline.tsv)'"

if [ "$failures" -ne 0 ]; then
    echo "$failures legacy-ratchet test(s) failed" >&2
    exit 1
fi
echo "legacy-ratchet tests passed"
