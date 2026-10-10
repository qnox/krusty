#!/bin/sh
# Installs the krusty Build Tools API implementation into a Kotlin Toolchain shared cache, as the
# org.jetbrains.kotlin:kotlin-build-tools-impl artifact of the Kotlin version the jar was built for.
#
# The Kotlin Toolchain loads the Build Tools API implementation by Maven coordinates and has no
# setting that names another one, so the cache entry is the hook. Use a dedicated cache directory:
# every project that uses this cache and that Kotlin version then compiles with krusty.
#
#   usage: install-into-kotlin-cache.sh <krusty-build-tools jar> <shared cache dir>
#   then:  KOTLIN_SHARED_CACHE_DIR=<shared cache dir> KRUSTY_BIN=<krusty binary> ./kotlin build
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <krusty-build-tools jar> <shared cache dir>" >&2
  exit 2
fi
jar=$1
cache=$2

version=$(unzip -p "$jar" krusty/buildtools/krusty-build-tools.properties | sed -n 's/^kotlin\.version=//p')
if [ -z "$version" ]; then
  echo "error: $jar does not name the Kotlin version it implements" >&2
  exit 1
fi

dir="$cache/.m2.cache/org/jetbrains/kotlin/kotlin-build-tools-impl/$version"
mkdir -p "$dir"
rm -f "$dir"/*
cp "$jar" "$dir/kotlin-build-tools-impl-$version.jar"
cat > "$dir/kotlin-build-tools-impl-$version.pom" <<POM
<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>org.jetbrains.kotlin</groupId>
  <artifactId>kotlin-build-tools-impl</artifactId>
  <version>$version</version>
  <description>krusty Build Tools API implementation</description>
</project>
POM
for file in "$dir"/*.jar "$dir"/*.pom; do
  sha1sum "$file" | cut -d' ' -f1 > "$file.sha1"
done
echo "installed krusty as kotlin-build-tools-impl $version in $cache"
