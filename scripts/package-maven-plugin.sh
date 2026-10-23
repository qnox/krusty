#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 VERSION OUTPUT_DIRECTORY" >&2
  exit 2
fi

version=$1
output=$2
if [[ ! $version =~ ^[0-9A-Za-z][0-9A-Za-z._+-]*$ ]]; then
  echo "invalid Maven plugin version: $version" >&2
  exit 2
fi

repo=$(cd "$(dirname "$0")/.." && pwd)
project=$repo/tools/krusty-maven
mkdir -p "$output"
output=$(cd "$output" && pwd)

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

root=$stage/krusty-maven-plugin-$version
mkdir -p "$root/repository"
# A Maven repository layout holding exactly this plugin, deployed by Maven itself.
mvn -B -f "$project/pom.xml" -Drevision="$version" clean package \
  org.apache.maven.plugins:maven-deploy-plugin:3.1.3:deploy \
  -DaltDeploymentRepository="release::file://$root/repository"

artifact=$root/repository/dev/krustythecompiler/krusty-maven-plugin/$version
built_jar=$project/target/krusty-maven-plugin-$version.jar
test -f "$artifact/krusty-maven-plugin-$version.jar"
test -f "$artifact/krusty-maven-plugin-$version.pom"
cmp "$built_jar" "$artifact/krusty-maven-plugin-$version.jar"
unzip -p "$built_jar" META-INF/maven/plugin.xml | grep -q '<goalPrefix>krusty</goalPrefix>'
unzip -p "$built_jar" META-INF/MANIFEST.MF | grep -q "^Implementation-Version: $version"
grep -q "<version>$version</version>" "$artifact/krusty-maven-plugin-$version.pom"
if grep -qF '${revision}' "$artifact/krusty-maven-plugin-$version.pom"; then
  echo "the deployed POM still names \${revision}" >&2
  exit 1
fi
cp "$repo/LICENSE" "$root/LICENSE"

cat > "$root/README.md" <<README
# krusty Maven plugin $version

This is a portable local Maven repository holding the krusty Maven plugin. Unpack it beside the
consuming build, add it as a plugin repository, and run the verify goal after kotlin-maven-plugin
has compiled:

\`\`\`xml
<pluginRepositories>
  <pluginRepository>
    <id>krusty</id>
    <url>file:///path/to/krusty-maven-plugin-$version/repository</url>
  </pluginRepository>
</pluginRepositories>
\`\`\`

\`\`\`sh
mvn test-compile dev.krustythecompiler:krusty-maven-plugin:$version:verify
\`\`\`

The goal downloads the krusty $version compiler for the host from its GitHub release and checks it
against the release's SHA256SUMS; \`-Dkrusty.binary=/path/to/krusty\` runs another binary instead.
kotlinc still builds what Maven ships. See tools/krusty-maven/README.md in the krusty repository.
README

(
  cd "$root"
  find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS
  # ZIP stores DOS timestamps. Fix every entry before archiving and feed zip a stable file order.
  find . -exec touch -h -d '1980-01-01 00:00:00 UTC' {} +
)

jar_asset=$output/krusty-maven-plugin-$version.jar
zip_asset=$output/krusty-maven-plugin-$version.zip
checksums=$output/krusty-maven-plugin-$version-SHA256SUMS
cp "$built_jar" "$jar_asset"
(
  cd "$stage"
  find "krusty-maven-plugin-$version" -type f -print | LC_ALL=C sort \
    | zip -X -q "$zip_asset" -@
)
(
  cd "$output"
  sha256sum "$(basename "$jar_asset")" "$(basename "$zip_asset")" > "$(basename "$checksums")"
)
printf '%s\n' "$zip_asset"
