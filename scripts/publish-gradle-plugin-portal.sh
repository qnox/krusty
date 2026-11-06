#!/usr/bin/env bash
# Plugin Portal versions are immutable. A retry must read the exact version before uploading;
# an HTTP/authentication failure is not evidence that the version is absent.
# Portal Maven2 repository/redirects: https://plugins.gradle.org/docs/mirroring
# Marker coordinates: https://docs.gradle.org/current/userguide/plugins_intermediate.html#sec:plugin_markers
set -euo pipefail
[[ $# -eq 1 ]] || { echo "usage: $0 VERSION" >&2; exit 2; }
version=$1
repo=$(cd "$(dirname "$0")/.." && pwd)
marker="https://plugins.gradle.org/m2/dev/krustythecompiler/dev.krustythecompiler.gradle.plugin/$version/dev.krustythecompiler.gradle.plugin-$version.pom"
code=$(curl --silent --show-error --location --connect-timeout 10 --max-time 60 \
  --output /dev/null --write-out '%{http_code}' "$marker")
case $code in
  200) echo "krusty-gradle-plugin $version is already on the Plugin Portal"; exit 0 ;;
  404) ;;
  *) echo "cannot determine Plugin Portal publication of $version (HTTP $code)" >&2; exit 1 ;;
esac
"$repo/tools/krusty-gradle/gradlew" --no-daemon -p "$repo/tools/krusty-gradle" \
  -PkrustyPluginVersion="$version" publishPlugins
