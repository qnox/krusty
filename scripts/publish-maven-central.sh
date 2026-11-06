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
# RELEASE_SHA (GITHUB_SHA, then HEAD) identifies the source commit whose author owns the record.
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
# Only public release context belongs in progress logs. Never print request URLs, deployment
# records or Central response bodies. The owner alone removes the shared private working files.
log_owner=$BASHPID
log_version=unavailable
if [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9]+([.-][A-Za-z0-9]+)*)?$ ]]; then
  log_version=$version
fi
log_source=$(git -C "$repo" rev-parse --verify --end-of-options "${RELEASE_SHA:-${GITHUB_SHA:-HEAD}}^{commit}" 2>/dev/null) || log_source=unavailable
log_stage=setup
log_started=$(date +%s)

log_event() {
  local elapsed=$(($(date +%s) - log_started))
  ((elapsed >= 0)) || elapsed=0
  printf '[release] stage=%s version=%s source=%s elapsed_seconds=%s outcome=%s exit=%s\n' \
    "$log_stage" "$log_version" "$log_source" "$elapsed" "$1" "$2" >&2
}

finish() {
  local actual=$?
  trap - EXIT
  if [[ $actual -ne 0 && ! -e $stage/failure-logged ]]; then
    log_event failed "$actual"
    : > "$stage/failure-logged"
  fi
  if [[ $BASHPID == "$log_owner" ]]; then
    rm -rf "$stage"
  fi
  exit "$actual"
}
trap finish EXIT

begin_stage() {
  log_stage=$1
  log_started=$(date +%s)
  # Command substitutions have their own EXIT trap and cannot remove the owner's auth/bundle.
  trap finish EXIT
  log_event start 0
}

end_stage() {
  log_event done 0
}

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
  # Curl writes only fixed transfer metadata to stderr; the response stdout remains the input
  # to the existing caller. Silent mode suppresses raw transport errors and response text.
  curl -fs --connect-timeout "$((limit < 10 ? limit : 10))" --max-time "$limit" -H "@$auth" \
    --write-out "%{stderr}[release] stage=$log_stage version=$log_version source=$log_source elapsed_seconds=%{time_total} outcome=http exit=%{exitcode} http_status=%{http_code}\n" "$@"
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
  git -C "$repo" ls-remote --exit-code "$remote" "$deployment_ref" > /dev/null 2>&1 || listed=$?
  case $listed in
    0) ;;
    2) return ;;
    *)
      echo "cannot read deployment journal (git ls-remote exited $listed)" >&2
      exit 1
      ;;
  esac
  if ! git -C "$repo" fetch --quiet --no-tags "$remote" "+$deployment_ref:refs/central-publisher/fetched" 2>/dev/null; then
    echo "cannot fetch deployment journal" >&2
    exit 1
  fi
  recorded=$(git -C "$repo" rev-parse refs/central-publisher/fetched)
  deployment=$(git -C "$repo" log -1 --format=%s "$recorded" |
    sed -n "s/^central deployment \([^ ]\{1,\}\) of krusty-gradle-plugin $version\$/\1/p")
  if [[ -z $deployment ]]; then
    echo "deployment journal names no deployment" >&2
    exit 1
  fi
}

resolve_source_identity() {
  local source_commit
  source_commit=$(git -C "$repo" rev-parse --verify --end-of-options "${RELEASE_SHA:-${GITHUB_SHA:-HEAD}}^{commit}" 2>/dev/null)
  author_name=$(git -C "$repo" show -s --format=%an "$source_commit")
  author_email=$(git -C "$repo" show -s --format=%ae "$source_commit")
}

record_deployment() {
  local commit
  commit=$(GIT_AUTHOR_NAME="$author_name" GIT_AUTHOR_EMAIL="$author_email" \
    GIT_COMMITTER_NAME="$author_name" GIT_COMMITTER_EMAIL="$author_email" \
    git -C "$repo" commit-tree "$(git -C "$repo" hash-object -t tree /dev/null)" \
    -m "central deployment $1 of krusty-gradle-plugin $version")
  git -C "$repo" push --quiet --force-with-lease="$deployment_ref:$recorded" "$remote" "$commit:$deployment_ref" 2>/dev/null
}

upload() {
  begin_stage gradle-build
  # The build signs; it has no use for the Portal token.
  env -u MAVEN_CENTRAL_USERNAME -u MAVEN_CENTRAL_PASSWORD "$project/gradlew" --no-daemon -p "$project" -PkrustyPluginVersion="$version" \
    clean publishAllPublicationsToReleaseRepository >&2
  end_stage
  begin_stage bundle
  (
    cd "$project/build/release-repository"
    find . -type f ! -name 'maven-metadata.xml*' | LC_ALL=C sort > "$stage/files"
    # Central rejects a bundle whose artifacts are not each signed.
    missing=$(grep -vE '\.(asc|md5|sha1|sha256|sha512)$' "$stage/files" |
      while read -r f; do [[ -f $f.asc ]] || echo "$f"; done)
    if [[ -n $missing ]]; then
      echo "release repository contains unsigned artifacts" >&2
      exit 1
    fi
    zip -X -q "$stage/bundle.zip" -@ < "$stage/files"
  )
  end_stage
  begin_stage upload
  request "$upload_limit" -F "bundle=@$stage/bundle.zip" \
    "$api/upload?name=krusty-gradle-plugin-$version&publishingType=AUTOMATIC"
  end_stage
}

begin_stage availability
implementation=$(published krusty-gradle-plugin)
marker=$(published dev.krustythecompiler.gradle.plugin)
end_stage
if [[ $implementation == true && $marker == true ]]; then
  echo "krusty-gradle-plugin $version is already on Maven Central"
  exit 0
fi
if [[ $implementation == true || $marker == true ]]; then
  echo "only one of the implementation and marker is on Maven Central" >&2
  exit 1
fi

recorded=
deployment=
begin_stage journal-read
read_record
end_stage
if [[ -n $deployment ]]; then
  begin_stage resume-status
  if [[ $(status "$deployment" | jq -r '.deploymentState') == FAILED ]]; then
    echo "recorded deployment failed; uploading again"
    deployment=
  else
    echo "following recorded deployment"
  fi
  end_stage
fi
if [[ -z $deployment ]]; then
  # Validate provenance before a build or an irreversible Portal upload.
  begin_stage source-check
  resolve_source_identity
  end_stage
  begin_stage publication
  deployment=$(upload)
  end_stage
  begin_stage journal-write
  record_deployment "$deployment"
  end_stage
  echo "uploaded deployment"
fi

# Validation takes a minute or two; propagation to the repository after PUBLISHING takes longer
# and needs no watching.
begin_stage validation
deadline=$(($(date +%s) + wait_limit))
state=unreported
while :; do
  remaining=$((deadline - $(date +%s)))
  if [[ $remaining -le 0 ]]; then
    echo "Maven Central validation did not finish after ${wait_limit}s" >&2
    exit 1
  fi
  result=$(status "$deployment" "$((remaining < request_limit ? remaining : request_limit))")
  state=$(jq -r '.deploymentState' <<<"$result")
  case $state in
    PUBLISHING | PUBLISHED)
      echo "deployment is $state"
      end_stage
      exit 0
      ;;
    FAILED)
      echo "Maven Central rejected deployment" >&2
      exit 1
      ;;
  esac
  remaining=$((deadline - $(date +%s)))
  if [[ $remaining -gt 0 ]]; then
    sleep "$((remaining < poll ? remaining : poll))"
  fi
done
