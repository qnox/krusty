#!/usr/bin/env bash
# Publish one release of the Gradle plugin (its implementation and plugin marker) to Maven Central
# through the Central Portal. The signed publications are built into the plugin's local release
# repository, bundled without its maven-metadata files, uploaded for automatic publication, and
# followed until the Portal accepts them for publishing. A version Central already has is skipped,
# so a rerun of a release whose upload succeeded does not fail on Central's immutable versions.
#
#   MAVEN_CENTRAL_USERNAME=... MAVEN_CENTRAL_PASSWORD=... \
#   ORG_GRADLE_PROJECT_signingInMemoryKey=... ORG_GRADLE_PROJECT_signingInMemoryKeyId=... \
#   ORG_GRADLE_PROJECT_signingInMemoryKeyPassword=... scripts/publish-maven-central.sh VERSION
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 VERSION" >&2
  exit 2
fi
version=$1
: "${MAVEN_CENTRAL_USERNAME:?}" "${MAVEN_CENTRAL_PASSWORD:?}" "${ORG_GRADLE_PROJECT_signingInMemoryKey:?}"

api=https://central.sonatype.com/api/v1/publisher
auth="Authorization: Bearer $(printf '%s:%s' "$MAVEN_CENTRAL_USERNAME" "$MAVEN_CENTRAL_PASSWORD" | base64 -w0)"
repo=$(cd "$(dirname "$0")/.." && pwd)
project=$repo/tools/krusty-gradle

published=$(curl -fsS -H "$auth" \
  "$api/published?namespace=dev.krustythecompiler&name=krusty-gradle-plugin&version=$version")
if jq -e '.published' <<<"$published" >/dev/null; then
  echo "dev.krustythecompiler:krusty-gradle-plugin:$version is already on Maven Central"
  exit 0
fi

"$project/gradlew" --no-daemon -p "$project" -PkrustyPluginVersion="$version" \
  clean publishAllPublicationsToReleaseRepository

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
(
  cd "$project/build/release-repository"
  find . -type f ! -name 'maven-metadata.xml*' | LC_ALL=C sort > "$stage/files"
  # Central rejects a bundle whose artifacts are not each signed.
  missing=$(grep -vE '\.(asc|md5|sha1|sha256|sha512)$' "$stage/files" |
    while read -r f; do [[ -f $f.asc ]] || echo "$f"; done)
  if [[ -n $missing ]]; then
    echo "unsigned artifacts:" >&2
    echo "$missing" >&2
    exit 1
  fi
  zip -X -q "$stage/bundle.zip" -@ < "$stage/files"
)

deployment=$(curl -fsS -H "$auth" -F "bundle=@$stage/bundle.zip" \
  "$api/upload?name=krusty-gradle-plugin-$version&publishingType=AUTOMATIC")
echo "uploaded deployment $deployment"

# Validation takes a minute or two; propagation to the repository after PUBLISHING takes longer
# and needs no watching.
for _ in $(seq 30); do
  status=$(curl -fsS -X POST -H "$auth" "$api/status?id=$deployment")
  state=$(jq -r '.deploymentState' <<<"$status")
  case $state in
    PUBLISHING | PUBLISHED)
      echo "deployment $deployment is $state"
      exit 0
      ;;
    FAILED)
      echo "deployment $deployment failed:" >&2
      jq '.errors' <<<"$status" >&2
      exit 1
      ;;
  esac
  sleep 10
done
echo "deployment $deployment is still $state after 5 minutes" >&2
exit 1
