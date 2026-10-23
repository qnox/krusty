#!/usr/bin/env bash
# Publish one release of the Gradle plugin (its implementation and plugin marker) to Maven Central
# through the Central Portal. The signed publications are built into the plugin's local release
# repository, bundled without its maven-metadata files, uploaded for automatic publication, and
# followed until the Portal accepts them for publishing.
#
# Central's versions are immutable, and a deployment the Portal accepted keeps PUBLISHING for a
# while before the published check sees it. So a rerun must find that deployment rather than
# upload again: each upload's id is recorded in the remote ref refs/krusty/central/<version>, and a
# rerun follows the recorded deployment, uploading again only when it FAILED. A version whose
# implementation and marker are both published is skipped; one with only one of them published
# cannot be repaired by an upload and fails.
#
# Every request has a deadline, and following a deployment gives up after five minutes.
#
#   MAVEN_CENTRAL_USERNAME=... MAVEN_CENTRAL_PASSWORD=... \
#   ORG_GRADLE_PROJECT_signingInMemoryKey=... ORG_GRADLE_PROJECT_signingInMemoryKeyId=... \
#   ORG_GRADLE_PROJECT_signingInMemoryKeyPassword=... scripts/publish-maven-central.sh VERSION
#
# Environment: RELEASE_REMOTE (origin), the remote holding the deployment refs.
set -euo pipefail
shopt -s inherit_errexit

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
remote=${RELEASE_REMOTE:-origin}
deployment_ref=refs/krusty/central/$version
wait_limit=300
poll=10
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

request() {
  curl -fsS --connect-timeout 10 --max-time 60 -H "$auth" "$@"
}

published() {
  request "$api/published?namespace=dev.krustythecompiler&name=$1&version=$version" | jq -r '.published'
}

record_deployment() {
  local commit
  commit=$(GIT_AUTHOR_NAME=github-actions GIT_AUTHOR_EMAIL=github-actions@users.noreply.github.com \
    GIT_COMMITTER_NAME=github-actions GIT_COMMITTER_EMAIL=github-actions@users.noreply.github.com \
    git -C "$repo" commit-tree "$(git -C "$repo" hash-object -t tree /dev/null)" \
    -m "central deployment $1 of krusty-gradle-plugin $version")
  git -C "$repo" push --quiet --force-with-lease="$deployment_ref:$recorded" "$remote" "$commit:$deployment_ref"
}

status() {
  request -X POST "$api/status?id=$1"
}

upload() {
  "$project/gradlew" --no-daemon -p "$project" -PkrustyPluginVersion="$version" \
    clean publishAllPublicationsToReleaseRepository >&2
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
  curl -fsS --connect-timeout 10 --max-time 120 -H "$auth" -F "bundle=@$stage/bundle.zip" \
    "$api/upload?name=krusty-gradle-plugin-$version&publishingType=AUTOMATIC"
}

implementation=$(published krusty-gradle-plugin)
marker=$(published dev.krustythecompiler.gradle.plugin)
if [[ $implementation == true && $marker == true ]]; then
  echo "krusty-gradle-plugin $version is already on Maven Central"
  exit 0
fi
if [[ $implementation == true || $marker == true ]]; then
  echo "only one of the implementation ($implementation) and marker ($marker) of $version is on Maven Central" >&2
  exit 1
fi

# The deployment recorded for this version, if any: its ref's commit and its id.
recorded=
deployment=
if git -C "$repo" fetch --quiet --no-tags "$remote" "+$deployment_ref:refs/central-publisher/fetched" 2>/dev/null; then
  recorded=$(git -C "$repo" rev-parse refs/central-publisher/fetched)
  deployment=$(git -C "$repo" log -1 --format=%s "$recorded" | sed -n 's/^central deployment \([^ ]*\) .*/\1/p')
fi

if [[ -n $deployment ]]; then
  if [[ $(status "$deployment" | jq -r '.deploymentState') == FAILED ]]; then
    echo "recorded deployment $deployment failed; uploading again"
    deployment=
  else
    echo "following recorded deployment $deployment"
  fi
fi
if [[ -z $deployment ]]; then
  deployment=$(upload)
  record_deployment "$deployment"
  echo "uploaded deployment $deployment"
fi

# Validation takes a minute or two; propagation to the repository after PUBLISHING takes longer
# and needs no watching.
deadline=$(($(date +%s) + wait_limit))
while :; do
  result=$(status "$deployment")
  state=$(jq -r '.deploymentState' <<<"$result")
  case $state in
    PUBLISHING | PUBLISHED)
      echo "deployment $deployment is $state"
      exit 0
      ;;
    FAILED)
      echo "deployment $deployment failed:" >&2
      jq '.errors' <<<"$result" >&2
      exit 1
      ;;
  esac
  if [[ $(date +%s) -ge $deadline ]]; then
    echo "deployment $deployment is still $state after ${wait_limit}s" >&2
    exit 1
  fi
  sleep "$poll"
done
