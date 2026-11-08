#!/usr/bin/env bash
# Central must retrieve the artifact signer's public certificate from a supported keyserver.
# Use only the public certificate already exported and verified by sign-release-checksums.sh.
# Check the actual signing subkey, since a server may know the root but lack a new subkey.
#
#   scripts/publish-release-signing-key.sh VERSION DIST
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 VERSION DIST" >&2
  exit 2
fi
version=$1
dist=$2
repo=$(cd "$(dirname "$0")/.." && pwd)
root_key=${KRUSTY_RELEASE_ROOT_KEY:-$repo/tools/krusty-gradle/src/main/resources/krusty/release-root-key.asc}
certificate=$dist/krusty-$version-signing-key.asc
sums=$dist/krusty-$version-SHA256SUMS
server=https://keyserver.ubuntu.com
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -m 700 "$stage/local" "$stage/remote"

primary() {
  gpg --homedir "$stage/local" --batch --with-colons --show-keys "$1" 2>/dev/null |
    awk -F: '$1 == "fpr" { print $10; exit }'
}
root=$(primary "$root_key")
if [[ ! $root =~ ^[0-9A-F]{40,64}$ || $(primary "$certificate") != "$root" ]]; then
  echo "release signing certificate does not match the pinned root" >&2
  exit 1
fi
gpg --homedir "$stage/local" --batch --quiet --import "$certificate" 2>/dev/null
# Re-export the pinned certificate: this file can contain only public packets for that root.
gpg --homedir "$stage/local" --batch --quiet --armor --export "$root" > "$stage/public.asc"
gpg --homedir "$stage/local" --batch --no-auto-key-retrieve --status-fd 1 \
  --verify "$sums.asc" "$sums" > "$stage/status" 2>/dev/null
signer=$(awk -v root="$root" '$2 == "VALIDSIG" && $NF == root { print $3 }' "$stage/status")
if [[ ! $signer =~ ^[0-9A-F]{40,64}$ ]]; then
  echo "release checksum signature does not identify one signer of the pinned root" >&2
  exit 1
fi

lookup() {
  local code
  code=$(curl --silent --show-error --connect-timeout 10 --max-time 30 \
    --output "$stage/download.asc" --write-out '%{http_code}' \
    "$server/pks/lookup?op=get&search=0x$signer") || return 2
  case "$code" in
    404) return 1 ;;
    200) ;;
    *) echo "release public-key lookup failed: HTTP $code" >&2; return 2 ;;
  esac
  gpg --homedir "$stage/remote" --batch --quiet --import "$stage/download.asc" 2>/dev/null || return 2
  gpg --homedir "$stage/remote" --batch --no-auto-key-retrieve --status-fd 1 \
    --verify "$sums.asc" "$sums" > "$stage/remote-status" 2>/dev/null || return 2
  awk -v root="$root" -v signer="$signer" \
    '$2 == "VALIDSIG" && $3 == signer && $NF == root { valid = 1 } END { exit !valid }' "$stage/remote-status" || return 2
}

status=0
lookup || status=$?
if [[ $status == 2 ]]; then
  echo "could not verify the release signer through the public keyserver" >&2
  exit 1
fi
if [[ $status == 1 ]]; then
  echo "publishing public release signer $signer to keyserver.ubuntu.com"
  curl --silent --show-error --fail --connect-timeout 10 --max-time 30 \
    --output "$stage/upload-response" --data-urlencode "keytext@$stage/public.asc" "$server/pks/add"
  for attempt in {1..6}; do
    status=0
    lookup || status=$?
    [[ $status == 0 ]] && break
    [[ $status == 1 && $attempt != 6 ]] || break
    sleep 10
  done
  if [[ $status != 0 ]]; then
    echo "release signer is not yet verifiable through the public keyserver" >&2
    exit 1
  fi
fi
echo "public release signer $signer is retrievable and verifies the release checksums"
