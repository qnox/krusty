#!/usr/bin/env bash
# Exercise scripts/signing-key-id.sh against keys generated in a disposable keyring: the release's
# signing-subkey export resolves to its subkey's short id, and every export without exactly one
# usable signing key is rejected.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'gpgconf --homedir "$fixture/home" --kill gpg-agent 2>/dev/null || true; rm -rf "$fixture"' EXIT
mkdir -m 700 "$fixture/home"
gpg_() { gpg --homedir "$fixture/home" --batch --quiet --pinentry-mode loopback --passphrase fixture "$@"; }

gpg_ --quick-gen-key 'Signing fixture <fixture@example.invalid>' ed25519 cert never 2>/dev/null
primary=$(gpg_ --with-colons --list-keys | awk -F: '$1 == "fpr" { print $10; exit }')
subkeys() { gpg_ --with-colons --list-keys | awk -F: -v cap="$1" '$1 == "sub" && $12 ~ cap { print $5 }'; }
gpg_ --quick-add-key "$primary" ed25519 sign never
gpg_ --quick-add-key "$primary" cv25519 encr never
sign=$(subkeys s)
encr=$(subkeys e)
gpg_ --armor --export-secret-subkeys "$sign!" > "$fixture/sign.asc"
gpg_ --armor --export-secret-subkeys "$encr!" > "$fixture/encr.asc"
gpg_ --armor --export "$primary" > "$fixture/public.asc"
gpg_ --quick-add-key "$primary" ed25519 sign never
gpg_ --armor --export-secret-keys "$primary" > "$fixture/full.asc"
second=$(subkeys s | grep -v "$sign")
echo 'not a key' > "$fixture/junk.asc"
: > "$fixture/empty.asc"

failures=0
expect() {
    local name=$1 status=$2 stdout=$3 stderr=$4 actual_status=0
    "$repo/scripts/signing-key-id.sh" < "$fixture/$name.asc" > "$fixture/out" 2> "$fixture/err" || actual_status=$?
    if [[ $actual_status != "$status" || $(cat "$fixture/out") != "$stdout" || $(cat "$fixture/err") != "$stderr" ]]; then
        echo "$name: expected status $status, stdout '$stdout', stderr '$stderr'" >&2
        echo "$name: got status $actual_status, stdout '$(cat "$fixture/out")', stderr '$(cat "$fixture/err")'" >&2
        failures=$((failures + 1))
    fi
}

expect sign 0 "${sign: -8}" ''
expect encr 1 '' 'the signing key holds 0 usable signing (sub)keys, expected exactly one'
expect public 1 '' 'the signing key holds 0 usable signing (sub)keys, expected exactly one'
expect full 1 '' "the signing key holds 2 usable signing (sub)keys, expected exactly one: $sign $second"
expect junk 1 '' 'the signing key is not an importable OpenPGP secret key'
expect empty 1 '' 'the signing key is not an importable OpenPGP secret key'

if [[ $failures -ne 0 ]]; then
    echo "$failures signing key id case(s) failed" >&2
    exit 1
fi
echo "signing key id: 6 cases passed"
