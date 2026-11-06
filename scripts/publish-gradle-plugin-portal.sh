#!/usr/bin/env bash
# Portal-owned artifacts prove publication; Central's copy of the marker does not.
# https://plugins.gradle.org/docs/mirroring documents Portal's 303 artifact origin.
set -euo pipefail
[[ $# -eq 1 ]] || { echo "usage: $0 VERSION" >&2; exit 2; }
version=$1
repo=$(cd "$(dirname "$0")/.." && pwd)
marker="https://plugins.gradle.org/m2/dev/krustythecompiler/dev.krustythecompiler.gradle.plugin/$version/dev.krustythecompiler.gradle.plugin-$version.pom"
headers=$(mktemp)
trap 'rm -f "$headers"' EXIT
request() {
  curl --silent --show-error --connect-timeout 10 --max-time 60 \
    --output /dev/null --write-out '%{http_code}' "$@"
}
code=$(request --dump-header "$headers" "$marker")
case $code in
  303)
    mapfile -t locations < <(awk 'tolower($1)=="location:" {sub(/^[^:]*:[ \t]*/, ""); sub(/\r$/, ""); print}' "$headers")
    [[ ${#locations[@]} -eq 1 ]] || { echo "ambiguous Plugin Portal redirect" >&2; exit 1; }
    case ${locations[0]} in
      https://plugins-artifacts.gradle.org/*)
        artifact=$(request "${locations[0]}")
        case $artifact in
          200) echo "krusty-gradle-plugin $version is already on the Plugin Portal"; exit 0 ;;
          404) ;;
          *) echo "cannot verify Plugin Portal artifact (HTTP $artifact)" >&2; exit 1 ;;
        esac
        ;;
      https://repo.maven.apache.org/maven2/*|https://repo1.maven.org/maven2/*) ;;
      *) echo "untrusted Plugin Portal redirect" >&2; exit 1 ;;
    esac
    ;;
  404) ;;
  *) echo "cannot determine Plugin Portal publication of $version (HTTP $code)" >&2; exit 1 ;;
esac
"$repo/tools/krusty-gradle/gradlew" --no-daemon -p "$repo/tools/krusty-gradle" \
  -PkrustyPluginVersion="$version" publishPlugins
