#!/usr/bin/env bash
# Print the 8-hex-digit short id of the one signing key in an armored OpenPGP secret key read from
# stdin, for Gradle's in-memory signing. The release key is a signing subkey exported with
# `gpg --armor --export-secret-subkeys <id>!`, so its primary key is only a stub and the subkey is
# the one that can sign. The key is imported into a throwaway keyring; importing needs no
# passphrase. Fails unless exactly one key carries both secret material and the signing capability.
#
#   scripts/signing-key-id.sh < key.asc
set -euo pipefail

home=$(mktemp -d)
trap 'gpgconf --homedir "$home" --kill gpg-agent 2>/dev/null || true; rm -rf "$home"' EXIT

if ! gpg --homedir "$home" --batch --quiet --import 2>/dev/null; then
  echo "the signing key is not an importable OpenPGP secret key" >&2
  exit 1
fi
# Colon listing: field 2 validity (r revoked, e expired), 5 key id, 12 capabilities (lowercase s
# signs), 15 secret material (# stub, + present).
mapfile -t ids < <(gpg --homedir "$home" --batch --with-colons --list-secret-keys |
  awk -F: '($1 == "sec" || $1 == "ssb") && $2 !~ /[re]/ && $12 ~ /s/ && $15 == "+" { print $5 }')

if [[ ${#ids[@]} -ne 1 ]]; then
  echo "the signing key holds ${#ids[@]} usable signing (sub)keys, expected exactly one${ids[*]:+: ${ids[*]}}" >&2
  exit 1
fi
echo "${ids[0]: -8}"
