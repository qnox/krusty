#!/usr/bin/env bash
# Every validated commit still on master's history publishes its immutable version. A per-commit
# lock serializes retries without blocking other releases during Gradle publishing. A separate,
# short promotion lock protects GitHub latest, badges, and the monotonic released ref.
#
#   release-publisher.sh acquire | release LOCK       per-commit publication lock
#   release-publisher.sh check                        publish=true for master ancestors
#   release-publisher.sh bind-tag TAG                 immutable tag bound to RELEASE_SHA
#   release-publisher.sh acquire-latest | release-latest LOCK
#   release-publisher.sh check-promotion               promote=true unless a newer release won
#   release-publisher.sh record                       advance the released ref with a lease
#
# Environment: RELEASE_SHA (GITHUB_SHA), RELEASE_REMOTE (origin), RELEASE_BRANCH (master),
# RELEASE_LOCK_TTL seconds (1800), RELEASE_LOCK_WAIT seconds (900), RELEASE_LOCK_POLL seconds (10).
set -euo pipefail
shopt -s inherit_errexit

remote=${RELEASE_REMOTE:-origin}
branch=${RELEASE_BRANCH:-master}
sha=${RELEASE_SHA:-${GITHUB_SHA:-}}
lock_ref=refs/krusty/release-commits/$sha
promotion_lock_ref=refs/krusty/release-lock
released_ref=refs/krusty/released
ttl=${RELEASE_LOCK_TTL:-1800}
wait_limit=${RELEASE_LOCK_WAIT:-900}
poll=${RELEASE_LOCK_POLL:-10}

usage() {
  echo "usage: $0 acquire|release LOCK|check|bind-tag TAG|acquire-latest|release-latest LOCK|check-promotion|record" >&2
  exit 2
}

# Missing is distinct from an unreadable remote: only an actual absent ref returns empty.
remote_ref() {
  local listed=0
  git ls-remote --exit-code "$remote" "$1" >/dev/null 2>&1 || listed=$?
  case $listed in
    0) ;;
    2) return ;;
    *) echo "release-publisher: cannot read $1 from $remote" >&2; return 1 ;;
  esac
  git fetch --quiet --no-tags "$remote" "+$1:refs/release-publisher/fetched" || return 1
  git rev-parse 'refs/release-publisher/fetched^{commit}'
}

acquire() {
  local lock holder age deadline source_commit author_name author_email
  source_commit=$(git rev-parse --verify --end-of-options "$sha^{commit}")
  author_name=$(git show -s --format=%an "$source_commit")
  author_email=$(git show -s --format=%ae "$source_commit")
  lock=$(GIT_AUTHOR_NAME="$author_name" GIT_AUTHOR_EMAIL="$author_email" \
    GIT_COMMITTER_NAME="$author_name" GIT_COMMITTER_EMAIL="$author_email" \
    git commit-tree "$(git hash-object -t tree /dev/null)" \
    -m "release lock for $sha (run ${GITHUB_RUN_ID:-local})" \
    -m "publication lease $(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')")
  deadline=$(($(date +%s) + wait_limit))
  while :; do
    if git push --quiet --force-with-lease="$lock_ref:" "$remote" "$lock:$lock_ref" 2>/dev/null; then
      echo "$lock"
      return
    fi
    holder=$(remote_ref "$lock_ref")
    if [ -n "$holder" ]; then
      age=$(($(date +%s) - $(git log -1 --format=%ct "$holder")))
      if [ "$age" -gt "$ttl" ] &&
        git push --quiet --force-with-lease="$lock_ref:$holder" "$remote" "$lock:$lock_ref" 2>/dev/null; then
        echo "release-publisher: took over a stale $(git log -1 --format=%s "$holder")" >&2
        echo "$lock"
        return
      fi
      echo "release-publisher: waiting for $(git log -1 --format=%s "$holder")" >&2
    fi
    if [ "$(date +%s)" -ge "$deadline" ]; then
      echo "release-publisher: gave up waiting for $lock_ref after ${wait_limit}s" >&2
      exit 1
    fi
    sleep "$poll"
  done
}

eligible() {
  local tip released
  tip=$(remote_ref "refs/heads/$branch") || return 2
  if [ -z "$tip" ] || ! git merge-base --is-ancestor "$sha" "$tip"; then
    echo "release-publisher: $sha is not on $branch at ${tip:-nothing}" >&2
    return 1
  fi
  released=$(remote_ref "$released_ref") || return 2
  if [ -n "$released" ] && ! git merge-base --is-ancestor "$released" "$tip"; then
    echo "release-publisher: $branch at $tip no longer descends from released $released" >&2
    return 2
  fi
}

check() {
  local status=0
  eligible || status=$?
  case $status in
    0) echo "publish=true" ;;
    1) echo "publish=false" ;;
    *) return "$status" ;;
  esac
}

check_promotion() {
  local released
  local status=0
  eligible || status=$?
  case $status in
    0) ;;
    1) echo "promote=false"; return ;;
    *) return "$status" ;;
  esac
  released=$(remote_ref "$released_ref") || return 2
  if [ -n "$released" ] && ! git merge-base --is-ancestor "$released" "$sha"; then
    echo "promote=false"
    return
  fi
  echo "promote=true"
}

bind_tag() {
  local tag_ref=refs/tags/$1 existing push_status push_output
  git check-ref-format "$tag_ref"
  eligible || return $?
  existing=$(remote_ref "$tag_ref") || return 2
  if [ -z "$existing" ]; then
    push_output=$(mktemp)
    if git push --quiet --force-with-lease="$tag_ref:" "$remote" "$sha:$tag_ref" >"$push_output" 2>&1; then
      rm -f "$push_output"
      return
    else
      push_status=$?
    fi
    if ! existing=$(remote_ref "$tag_ref"); then
      echo "release-publisher: cannot verify $tag_ref after git push exit $push_status" >&2
      head -c 4096 "$push_output" >&2
      printf '\n' >&2
      rm -f "$push_output"
      return 2
    fi
    if [ "$existing" = "$sha" ]; then
      rm -f "$push_output"
      return
    fi
    echo "release-publisher: git push for $tag_ref failed (exit $push_status):" >&2
    head -c 4096 "$push_output" >&2
    printf '\n' >&2
    rm -f "$push_output"
    if [ -z "$existing" ]; then
      echo "release-publisher: cannot create $tag_ref; ref remains absent" >&2
      return 1
    fi
  fi
  if [ "$existing" != "$sha" ]; then
    echo "release-publisher: $tag_ref belongs to ${existing:-nothing}, not $sha" >&2
    return 1
  fi
}

record() {
  local released
  eligible || return $?
  released=$(remote_ref "$released_ref") || return 2
  if [ -n "$released" ] && ! git merge-base --is-ancestor "$released" "$sha"; then
    echo "release-publisher: refusing to record $sha over newer $released" >&2
    exit 1
  fi
  git push --quiet --force-with-lease="$released_ref:$released" "$remote" "$sha:$released_ref"
}

release() {
  if ! git push --quiet --force-with-lease="$lock_ref:$1" "$remote" ":$lock_ref" 2>/dev/null; then
    echo "release-publisher: $lock_ref is no longer $1; leaving it" >&2
  fi
}

[ "$#" -ge 1 ] && [[ $sha =~ ^[0-9a-f]{40}$ ]] || usage
case $1 in
  acquire | check | record)
    [ "$#" -eq 1 ] || usage
    "$1"
    ;;
  acquire-latest)
    [ "$#" -eq 1 ] || usage
    lock_ref=$promotion_lock_ref
    acquire
    ;;
  check-promotion)
    [ "$#" -eq 1 ] || usage
    check_promotion
    ;;
  bind-tag)
    [ "$#" -eq 2 ] || usage
    bind_tag "$2"
    ;;
  release | release-latest)
    [ "$#" -eq 2 ] || usage
    if [ "$1" = release-latest ]; then lock_ref=$promotion_lock_ref; fi
    release "$2"
    ;;
  *) usage ;;
esac
