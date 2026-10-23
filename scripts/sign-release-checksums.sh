#!/usr/bin/env bash
# Sign a release's compiler checksums with the release signing subkey and publish the subkey's
# public certificate beside them, as the Gradle plugin reads them before trusting an archive:
#
#   <dist>/krusty-<version>-SHA256SUMS.asc     detached signature over the checksums
#   <dist>/krusty-<version>-signing-key.asc    the signing subkey, bound by the maintainer's key
#
# SIGNING_KEY is the armored `gpg --export-secret-subkeys <id>!` export and SIGNING_KEY_PASSWORD
# its passphrase; SIGNING_KEY_ID names the subkey, and without it scripts/signing-key-id.sh reads
# the id from the key. Before anything is
# published the result is checked the way the plugin checks it: the certificate's primary key must
# be the root key the plugin bundles (KRUSTY_RELEASE_ROOT_KEY overrides it for tests), and the
# signature must verify against the subkey that root binds. A key the plugin would reject fails here.
#
#   scripts/sign-release-checksums.sh <version> <dist>
set -euo pipefail

version=$1
dist=$2
repo=$(cd "$(dirname "$0")/.." && pwd)
root_key=${KRUSTY_RELEASE_ROOT_KEY:-$repo/tools/krusty-gradle/src/main/resources/krusty/release-root-key.asc}
sums=$dist/krusty-$version-SHA256SUMS
certificate=$dist/krusty-$version-signing-key.asc

if [[ -z "${SIGNING_KEY:-}" ]]; then
  echo "SIGNING_KEY is not set; refusing to publish compiler archives the Gradle plugin cannot verify" >&2
  exit 1
fi
# A pipe, not a here-string, which an older bash backs with a temporary file.
key_id=${SIGNING_KEY_ID:-$(printf '%s\n' "$SIGNING_KEY" | "$repo/scripts/signing-key-id.sh")}
test -f "$sums"

signer=$(mktemp -d)
verifier=$(mktemp -d)
trap 'for h in "$signer" "$verifier"; do gpgconf --homedir "$h" --kill gpg-agent 2>/dev/null || true; rm -rf "$h"; done' EXIT

printf '%s\n' "$SIGNING_KEY" | gpg --homedir "$signer" --batch --quiet --import 2>/dev/null
printf '%s' "${SIGNING_KEY_PASSWORD:-}" | gpg --homedir "$signer" --batch --quiet --pinentry-mode loopback \
  --passphrase-fd 0 --armor --detach-sign --local-user "$key_id!" --output "$sums.asc" "$sums"
gpg --homedir "$signer" --batch --quiet --armor --export "$key_id!" > "$certificate"

primary() { gpg --homedir "$verifier" --batch --with-colons --show-keys "$1" 2>/dev/null | awk -F: '$1 == "fpr" { print $10; exit }'; }
root=$(primary "$root_key")
if [[ "$(primary "$certificate")" != "$root" ]]; then
  echo "the signing subkey $key_id is not bound by the release root key $root" >&2
  exit 1
fi
gpg --homedir "$verifier" --batch --quiet --import "$certificate" 2>/dev/null
if ! gpg --homedir "$verifier" --batch --status-fd 1 --verify "$sums.asc" "$sums" 2>/dev/null |
  grep -q "^\[GNUPG:\] VALIDSIG .* $root\$"; then
  echo "the signature of $(basename "$sums") does not verify against the release root key $root" >&2
  exit 1
fi
echo "signed $(basename "$sums") with subkey $key_id of $root"
