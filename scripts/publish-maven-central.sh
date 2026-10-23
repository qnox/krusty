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
# cannot be repaired by an upload and fails. Missing evidence never authorizes an upload: a ref
# that cannot be read, or a record that does not name a deployment, stops the step.
#
# Every request has a deadline, and following a deployment ends five minutes after it starts:
# each status request and each pause between them fits in the time that remains.
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
repo=$(cd "$(dirname "$0")/.." && pwd)
project=$repo/tools/krusty-gradle
remote=${RELEASE_REMOTE:-origin}
deployment_ref=refs/krusty/central/$version
request_limit=60
upload_limit=120
wait_limit=300
poll=10
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# The Portal takes the user token as a bearer header. GitHub masks the secrets themselves, not
# this encoding of them, and a command line is readable by every process on the machine, so the
# encoding is masked and curl reads the header from a file only this user can read.
token=$(printf '%s:%s' "$MAVEN_CENTRAL_USERNAME" "$MAVEN_CENTRAL_PASSWORD" | base64 -w0)
if [[ -n ${GITHUB_ACTIONS:-} ]]; then
  echo "::add-mask::$token"
fi
auth=$stage/authorization
(
  umask 077
  printf 'Authorization: Bearer %s\n' "$token" > "$auth"
)
unset token

# request SECONDS CURL-ARGS...
request() {
  local limit=$1
  shift
  curl -fsS --connect-timeout "$((limit < 10 ? limit : 10))" --max-time "$limit" -H "@$auth" "$@"
}

published() {
  request "$request_limit" "$api/published?namespace=dev.krustythecompiler&name=$1&version=$version" |
    jq -r '.published'
}

# status ID [SECONDS]
status() {
  request "${2:-$request_limit}" -X POST "$api/status?id=$1"
}

# The commit and deployment id recorded for this version, both empty when the ref is absent. An
# unreadable ref or a record that names no deployment stops the step.
read_record() {
  local listed=0
  git -C "$repo" ls-remote --exit-code "$remote" "$deployment_ref" > /dev/null || listed=$?
  case $listed in
    0) ;;
    2) return ;;
    *)
      echo "cannot read $deployment_ref from $remote (git ls-remote exited $listed)" >&2
      exit 1
      ;;
  esac
  if ! git -C "$repo" fetch --quiet --no-tags "$remote" "+$deployment_ref:refs/central-publisher/fetched"; then
    echo "cannot fetch $deployment_ref from $remote" >&2
    exit 1
  fi
  recorded=$(git -C "$repo" rev-parse refs/central-publisher/fetched)
  deployment=$(git -C "$repo" log -1 --format=%s "$recorded" |
    sed -n "s/^central deployment \([^ ]\{1,\}\) of krusty-gradle-plugin $version\$/\1/p")
  if [[ -z $deployment ]]; then
    echo "$deployment_ref at $recorded names no deployment: $(git -C "$repo" log -1 --format=%s "$recorded")" >&2
    exit 1
  fi
}

record_deployment() {
  local commit
  commit=$(GIT_AUTHOR_NAME='Anton Efimchuk' GIT_AUTHOR_EMAIL=anton.efimchuk@gmail.com \
    GIT_COMMITTER_NAME='Anton Efimchuk' GIT_COMMITTER_EMAIL=anton.efimchuk@gmail.com \
    git -C "$repo" commit-tree "$(git -C "$repo" hash-object -t tree /dev/null)" \
    -m "central deployment $1 of krusty-gradle-plugin $version")
  git -C "$repo" push --quiet --force-with-lease="$deployment_ref:$recorded" "$remote" "$commit:$deployment_ref"
}

upload() {
  # The build signs; it has no use for the Portal token.
  env -u MAVEN_CENTRAL_USERNAME -u MAVEN_CENTRAL_PASSWORD "$project/gradlew" --no-daemon -p "$project" -PkrustyPluginVersion="$version" \
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
  request "$upload_limit" -F "bundle=@$stage/bundle.zip" \
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

recorded=
deployment=
read_record
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
state=unreported
while :; do
  remaining=$((deadline - $(date +%s)))
  if [[ $remaining -le 0 ]]; then
    echo "deployment $deployment is still $state after ${wait_limit}s" >&2
    exit 1
  fi
  result=$(status "$deployment" "$((remaining < request_limit ? remaining : request_limit))")
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
  remaining=$((deadline - $(date +%s)))
  if [[ $remaining -gt 0 ]]; then
    sleep "$((remaining < poll ? remaining : poll))"
  fi
done
