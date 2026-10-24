#!/usr/bin/env bash
# Exercise scripts/sign-release-checksums.sh against keys generated in a disposable keyring: a
# subkey of the release root key signs and publishes its certificate, and a missing key or a
# subkey of another root is refused before anything a consumer would trust is left behind.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'gpgconf --homedir "$fixture/home" --kill gpg-agent 2>/dev/null || true; rm -rf "$fixture"' EXIT
mkdir -m 700 "$fixture/home"
gpg_() { gpg --homedir "$fixture/home" --batch --quiet --pinentry-mode loopback --passphrase fixture "$@"; }

new_root() {
  gpg_ --quick-gen-key "$1 <fixture@example.invalid>" ed25519 cert never 2>/dev/null
  gpg_ --with-colons --list-keys "$1" | awk -F: '$1 == "fpr" { print $10; exit }'
}
signing_subkey() {
  gpg_ --quick-add-key "$1" ed25519 sign never
  gpg_ --with-colons --list-keys "$1" | awk -F: '$1 == "sub" && $12 ~ /s/ { id = $5 } END { print substr(id, 9) }'
}
root=$(new_root 'Release root')
other=$(new_root 'Other root')
sign=$(signing_subkey "$root")
foreign=$(signing_subkey "$other")
gpg_ --armor --export "$root" > "$fixture/root.asc"
gpg_ --armor --export-secret-subkeys "$sign!" > "$fixture/sign.asc"
gpg_ --armor --export-secret-subkeys "$foreign!" > "$fixture/foreign.asc"
version=2.4.21-build.7

failures=0
expect() {
  local name=$1 key=$2 id=$3 status=$4 stdout=$5 stderr=$6 actual_status=0
  local dist=$fixture/dist-$name
  mkdir -p "$dist"
  echo "0000  krusty-$version-x86_64-unknown-linux-gnu.tar.gz" > "$dist/krusty-$version-SHA256SUMS"
  SIGNING_KEY=$key SIGNING_KEY_ID=$id SIGNING_KEY_PASSWORD=fixture KRUSTY_RELEASE_ROOT_KEY=$fixture/root.asc \
    "$repo/scripts/sign-release-checksums.sh" "$version" "$dist" > "$fixture/out" 2> "$fixture/err" ||
    actual_status=$?
  if [[ $actual_status != "$status" || $(cat "$fixture/out") != "$stdout" || $(cat "$fixture/err") != "$stderr" ]]; then
    echo "$name: expected status $status, stdout '$stdout', stderr '$stderr'" >&2
    echo "$name: got status $actual_status, stdout '$(cat "$fixture/out")', stderr '$(cat "$fixture/err")'" >&2
    failures=$((failures + 1))
  fi
}

expect signed "$(cat "$fixture/sign.asc")" "$sign" 0 "signed krusty-$version-SHA256SUMS with subkey $sign of $root" ''
if ! gpg_ --verify "$fixture/dist-signed/krusty-$version-SHA256SUMS.asc" "$fixture/dist-signed/krusty-$version-SHA256SUMS" 2>/dev/null; then
  echo "signed: the published signature does not verify" >&2
  failures=$((failures + 1))
fi
if [[ "$(gpg_ --with-colons --show-keys "$fixture/dist-signed/krusty-$version-signing-key.asc" | awk -F: '$1 == "sub" { print substr($5, 9) }')" != "$sign" ]]; then
  echo "signed: the published certificate does not hold exactly the signing subkey" >&2
  failures=$((failures + 1))
fi
expect derived-id "$(cat "$fixture/sign.asc")" '' 0 "signed krusty-$version-SHA256SUMS with subkey $sign of $root" ''
expect foreign "$(cat "$fixture/foreign.asc")" "$foreign" 1 '' \
  "the signing subkey $foreign is not bound by the release root key $root"
expect missing '' "$sign" 1 '' \
  'SIGNING_KEY is not set; refusing to publish compiler archives the Gradle plugin cannot verify'

if [[ $failures -ne 0 ]]; then
  echo "$failures release checksum signing case(s) failed" >&2
  exit 1
fi
echo "release checksum signing: 4 cases passed"
