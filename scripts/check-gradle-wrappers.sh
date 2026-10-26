#!/usr/bin/env bash
# Check every committed gradle-wrapper.jar against the checksum Gradle publishes for the version its
# gradle-wrapper.properties names, and require that properties file to pin the distribution's own
# SHA-256 to the published one.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

published() {
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 "https://services.gradle.org/distributions/$1.sha256"
}

status=0
jars=$(git ls-files -- '*gradle-wrapper*.jar')
if [[ -z "$jars" ]]; then
  echo "no committed gradle-wrapper.jar" >&2
  exit 1
fi
while IFS= read -r jar; do
  properties="${jar%.jar}.properties"
  if [[ "$(basename "$jar")" != gradle-wrapper.jar || ! -f "$properties" ]]; then
    echo "$jar: not a wrapper jar beside its gradle-wrapper.properties" >&2
    status=1
    continue
  fi
  url=$(sed -n 's/^distributionUrl=//p' "$properties" | sed 's/\\:/:/g')
  if [[ ! $url =~ ^https://services\.gradle\.org/distributions/(gradle-[0-9A-Za-z.-]+-(bin|all))\.zip$ ]]; then
    echo "$properties: distributionUrl is not a services.gradle.org distribution: $url" >&2
    status=1
    continue
  fi
  distribution=${BASH_REMATCH[1]}
  version=${distribution#gradle-}
  version=${version%-*}

  expected=$(published "gradle-$version-wrapper.jar")
  actual=$(sha256sum "$jar" | cut -d' ' -f1)
  if [[ "$actual" != "$expected" ]]; then
    echo "$jar: SHA-256 $actual is not Gradle $version's wrapper jar ($expected)" >&2
    status=1
  fi

  expected=$(published "$distribution.zip")
  pinned=$(sed -n 's/^distributionSha256Sum=//p' "$properties")
  if [[ "$pinned" != "$expected" ]]; then
    echo "$properties: distributionSha256Sum '${pinned}' is not $distribution.zip's SHA-256 ($expected)" >&2
    status=1
  fi
done <<<"$jars"
exit "$status"
