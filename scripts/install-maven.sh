#!/usr/bin/env bash
# Install a pinned Apache Maven into a per-user directory and put it on the workflow's PATH.
#
# Usage: scripts/install-maven.sh <version>
#
# The distribution is verified against a SHA-512 pinned here, so neither mirror has to be trusted.
# An installed copy is reused, which lets a workflow cache the directory and never download at all;
# otherwise the download tries Maven Central, then the Apache archive, with bounded retries.
set -euo pipefail

version="${1:?usage: install-maven.sh <version>}"
case "$version" in
  3.9.11) sha512=bcfe4fe305c962ace56ac7b5fc7a08b87d5abd8b7e89027ab251069faebee516b0ded8961445d6d91ec1985dfe30f8153268843c89aa392733d1a3ec956c9978 ;;
  *) echo "install-maven: no pinned checksum for Maven $version; add its SHA-512 to this script" >&2; exit 1 ;;
esac

root="${KRUSTY_TOOLS_DIR:-$HOME/.cache/krusty-tools}/maven"
home="$root/apache-maven-$version"

if [ ! -x "$home/bin/mvn" ]; then
  mkdir -p "$root"
  archive="$(mktemp "$root/download.XXXXXX")"
  trap 'rm -f "$archive"' EXIT
  file="apache-maven-$version-bin.tar.gz"
  fetched=
  for url in \
    "https://repo1.maven.org/maven2/org/apache/maven/apache-maven/$version/$file" \
    "https://archive.apache.org/dist/maven/maven-3/$version/binaries/$file"; do
    for attempt in 1 2 3; do
      if curl -fsSL --connect-timeout 20 --max-time 300 -o "$archive" "$url" \
        && [ "$(sha512sum "$archive" | awk '{print $1}')" = "$sha512" ]; then
        fetched=1
        break 2
      fi
      echo "install-maven: attempt $attempt failed for $url" >&2
      sleep $((attempt * 5))
    done
  done
  [ -n "$fetched" ] || { echo "install-maven: could not download a verified Maven $version" >&2; exit 1; }
  staging="$(mktemp -d "$root/extract.XXXXXX")"
  tar -xzf "$archive" -C "$staging"
  rm -rf "$home"
  mv "$staging/apache-maven-$version" "$home"
  rmdir "$staging"
fi

[ -x "$home/bin/mvn" ] || { echo "install-maven: $home has no bin/mvn" >&2; exit 1; }
if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$home/bin" >> "$GITHUB_PATH"
fi
echo "$home"
