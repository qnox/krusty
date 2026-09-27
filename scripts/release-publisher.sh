#!/usr/bin/env bash
# Serialize master's release publication and keep it monotonic. Master CI runs overlap, so two
# release jobs can publish at once and an older commit can publish after a newer one. A GitHub
# concurrency group would serialize them, but it keeps one pending job and cancels the one it
# displaces, which cancels that whole CI run.
#
# The lock is a ref on the remote, created only if absent (a push with an empty lease is an atomic
# compare-and-swap on the server). Under the lock, a commit publishes only if it is still the tip
# of master and descends from the last published commit, which is recorded in a second ref once
# the release is out. A lock older than the TTL belongs to a job that can no longer be running (the
# release job's timeout is shorter) and is taken over.
#
#   scripts/release-publisher.sh acquire           take the lock; prints the lock commit
#   scripts/release-publisher.sh check             prints publish=true|false for $GITHUB_OUTPUT
#   scripts/release-publisher.sh record            record the commit as the last published one
#   scripts/release-publisher.sh release <lock>    drop the lock if it is still ours
#
# Environment: RELEASE_SHA (default $GITHUB_SHA), RELEASE_REMOTE (origin), RELEASE_BRANCH
# (master), RELEASE_LOCK_TTL seconds (1800), RELEASE_LOCK_WAIT seconds (900),
# RELEASE_LOCK_POLL seconds (10).
set -euo pipefail

remote=${RELEASE_REMOTE:-origin}
branch=${RELEASE_BRANCH:-master}
sha=${RELEASE_SHA:-${GITHUB_SHA:-}}
lock_ref=refs/krusty/release-lock
released_ref=refs/krusty/released
ttl=${RELEASE_LOCK_TTL:-1800}
wait_limit=${RELEASE_LOCK_WAIT:-900}
poll=${RELEASE_LOCK_POLL:-10}

usage() {
  sed -n '13,20p' "$0" >&2
  exit 2
}

# Prints the commit a remote ref names, or nothing when the ref does not exist.
remote_ref() {
  if git fetch --quiet --no-tags "$remote" "+$1:refs/release-publisher/fetched" 2>/dev/null; then
    git rev-parse refs/release-publisher/fetched
  fi
}

acquire() {
  local lock holder age deadline
  lock=$(GIT_AUTHOR_NAME=github-actions GIT_AUTHOR_EMAIL=github-actions@users.noreply.github.com \
    GIT_COMMITTER_NAME=github-actions GIT_COMMITTER_EMAIL=github-actions@users.noreply.github.com \
    git commit-tree "$(git hash-object -t tree /dev/null)" \
    -m "release lock for $sha (run ${GITHUB_RUN_ID:-local})")
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

check() {
  local tip released
  tip=$(remote_ref "refs/heads/$branch")
  if [ "$tip" != "$sha" ]; then
    echo "release-publisher: $branch is at ${tip:-nothing}, not $sha; a newer run publishes" >&2
    echo "publish=false"
    return
  fi
  released=$(remote_ref "$released_ref")
  if [ -n "$released" ] && ! git merge-base --is-ancestor "$released" "$sha"; then
    echo "release-publisher: $released was published and $sha does not descend from it" >&2
    echo "publish=false"
    return
  fi
  echo "publish=true"
}

record() {
  local released
  released=$(remote_ref "$released_ref")
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

[ "$#" -ge 1 ] || usage
case $1 in
  acquire | check | record)
    [ "$#" -eq 1 ] && [ -n "$sha" ] || usage
    "$1"
    ;;
  release)
    [ "$#" -eq 2 ] || usage
    release "$2"
    ;;
  *) usage ;;
esac
