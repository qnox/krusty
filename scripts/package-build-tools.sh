#!/usr/bin/env bash
# Build, test and package the krusty Build Tools API implementation for one exact Kotlin version.
#
#   usage: package-build-tools.sh VERSION KOTLIN_VERSION KRUSTY_BINARY OUTPUT_DIRECTORY
#
# The tests drive KRUSTY_BINARY. The jar is built twice from clean and must come out byte-identical,
# so the released bytes are the tested bytes and anyone can rebuild them. OUTPUT_DIRECTORY receives
# the jar, the cache installer and a SHA256SUMS file covering both; the jar's path is printed.
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 VERSION KOTLIN_VERSION KRUSTY_BINARY OUTPUT_DIRECTORY" >&2
  exit 2
fi

version=$1
kotlin=$2
krusty=$3
output=$4
for value in "$version" "$kotlin"; do
  if [[ ! $value =~ ^[0-9A-Za-z][0-9A-Za-z._+-]*$ ]]; then
    echo "invalid version: $value" >&2
    exit 2
  fi
done
if [[ ! -x $krusty ]]; then
  echo "not an executable krusty binary: $krusty" >&2
  exit 2
fi
krusty=$(cd "$(dirname "$krusty")" && pwd)/$(basename "$krusty")

repo=$(cd "$(dirname "$0")/.." && pwd)
project=$repo/tools/krusty-build-tools
mkdir -p "$output"
output=$(cd "$output" && pwd)

name=krusty-build-tools-$version-kotlin-$kotlin
built=$project/build/libs/$name.jar
gradle=("$project/gradlew" --no-daemon -p "$project"
  -PkrustyBuildToolsVersion="$version" -PkotlinVersion="$kotlin" -Pkrusty.binary="$krusty")

"${gradle[@]}" clean test jar
first=$(mktemp)
trap 'rm -f "$first"' EXIT
cp "$built" "$first"
"${gradle[@]}" clean jar
if ! cmp -s "$first" "$built"; then
  echo "two clean builds of $name.jar differ" >&2
  exit 1
fi

# The jar must name exactly the Kotlin version it was built and tested for.
recorded=$(unzip -p "$built" krusty/buildtools/krusty-build-tools.properties)
if [[ $recorded != "kotlin.version=$kotlin" ]]; then
  echo "$name.jar records '$recorded', expected kotlin.version=$kotlin" >&2
  exit 1
fi

installer=krusty-build-tools-$version-install-into-kotlin-cache.sh
cp "$built" "$output/$name.jar"
cp "$project/install-into-kotlin-cache.sh" "$output/$installer"
chmod +x "$output/$installer"
(
  cd "$output"
  sha256sum "$name.jar" "$installer" > "$name-SHA256SUMS"
)
printf '%s\n' "$output/$name.jar"
