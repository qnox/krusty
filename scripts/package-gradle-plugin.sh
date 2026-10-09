#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 VERSION OUTPUT_DIRECTORY" >&2
  exit 2
fi

version=$1
output=$2
if [[ ! $version =~ ^[0-9A-Za-z][0-9A-Za-z._+-]*$ ]]; then
  echo "invalid Gradle plugin version: $version" >&2
  exit 2
fi

repo=$(cd "$(dirname "$0")/.." && pwd)
project=$repo/tools/krusty-gradle
mkdir -p "$output"
output=$(cd "$output" && pwd)

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

"$project/gradlew" --no-daemon -p "$project" \
  -PkrustyPluginVersion="$version" \
  clean publishAllPublicationsToReleaseRepository

built_jar=$project/build/libs/krusty-gradle-plugin-$version.jar
published_jar=$project/build/release-repository/dev/krusty/krusty-gradle-plugin/$version/krusty-gradle-plugin-$version.jar
marker=$project/build/release-repository/krusty/krusty.gradle.plugin/$version/krusty.gradle.plugin-$version.pom
test -f "$built_jar"
test -f "$published_jar"
test -f "$marker"
cmp "$built_jar" "$published_jar"
unzip -p "$built_jar" META-INF/gradle-plugins/krusty.properties \
  | grep -qx 'implementation-class=krusty.KrustyKotlinPlugin'
grep -q '<groupId>dev.krusty</groupId>' "$marker"
grep -q '<artifactId>krusty-gradle-plugin</artifactId>' "$marker"
grep -q "<version>$version</version>" "$marker"

root=$stage/krusty-gradle-plugin-$version
mkdir -p "$root/repository"
cp -R "$project/build/release-repository/." "$root/repository/"
cp "$repo/LICENSE" "$root/LICENSE"

cat > "$root/README.md" <<EOF
# krusty Gradle plugin $version

This is a portable local Maven repository. Unpack it in the consuming build's \`gradle\`
directory; it creates \`gradle/krusty-gradle-plugin-$version\`. Then add:

\`\`\`kotlin
// settings.gradle.kts
pluginManagement {
    repositories {
        maven { url = uri("gradle/krusty-gradle-plugin-$version/repository") }
        gradlePluginPortal()
        mavenCentral()
    }
}
\`\`\`

\`\`\`kotlin
// build.gradle.kts
plugins {
    kotlin("jvm") version "2.4.20" // supported: 2.4.0, 2.4.10, 2.4.20
    id("krusty") version "$version"
}
\`\`\`

Plugin order is part of the supported boundary: apply Kotlin/JVM first, then krusty. The
kotlinx.serialization plugin (\`kotlin("plugin.serialization")\`) is supported in any order; krusty
receives the Kotlin Gradle plugin's compiler-plugin classpath and options as kotlinc would. The
compiler identifies plugin jars and directories from their registrar service entries; any unsupported
Kotlin compiler plugin is rejected regardless of artifact filename because krusty cannot preserve its
compiler semantics. KSP2 (\`com.google.devtools.ksp\`) is supported: it runs processors in its own
task and adds no compiler plugin to the compilation; krusty compiles the sources they generate
(KSP's Gradle plugin needs Gradle 8 or newer).
Projects applying Gradle's \`kotlin-dsl\` (such as \`buildSrc\`) and Kotlin
compile tasks outside any source set stay with kotlinc; the plugin logs each task it leaves.

Compile with \`./gradlew -Pkrusty.binary=/path/to/krusty krustyCompile\`.
The Kotlin/JVM plugin is an explicit consumer dependency and is not bundled here. This local-repo
distribution does not claim availability from the Gradle Plugin Portal. A Build Tools API
\`compilerVersion\` override is rejected; use the compiler version supplied by the selected Kotlin
Gradle plugin.
Validated Gradle boundaries: 7.6.3, 8.14.3, 9.5.0, and 9.7.0.
EOF

cat > "$root/manifest.txt" <<EOF
plugin-id=krusty
plugin-version=$version
implementation=dev.krusty:krusty-gradle-plugin:$version
marker=krusty:krusty.gradle.plugin:$version
kotlin-gradle-plugin=2.4.0,2.4.10,2.4.20
gradle=7.6.3,8.14.3,9.5.0,9.7.0
EOF

(
  cd "$root"
  find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS
  # ZIP stores DOS timestamps. Fix every entry before archiving and feed zip a stable file order.
  find . -exec touch -h -d '1980-01-01 00:00:00 UTC' {} +
)

jar_asset=$output/krusty-gradle-plugin-$version.jar
zip_asset=$output/krusty-gradle-plugin-$version.zip
checksums=$output/krusty-gradle-plugin-$version-SHA256SUMS
cp "$built_jar" "$jar_asset"
(
  cd "$stage"
  find "krusty-gradle-plugin-$version" -type f -print | LC_ALL=C sort \
    | zip -X -q "$zip_asset" -@
)
(
  cd "$output"
  sha256sum "$(basename "$jar_asset")" "$(basename "$zip_asset")" > "$(basename "$checksums")"
)
printf '%s\n' "$zip_asset"
