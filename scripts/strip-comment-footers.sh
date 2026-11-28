#!/usr/bin/env bash
# Remove the footer an integration appends to the comments it posts, as check-attribution.sh
# --strip-footer removes it from a pull request body. An integration that appends a footer also
# appends it again when its own account edits the comment, so the footer is removed here with the
# workflow's token instead.
#
#   scripts/strip-comment-footers.sh --comment <api-path>  one issue comment, review comment or
#                                                          review, by its REST API path
#   scripts/strip-comment-footers.sh --since <timestamp>   every issue and review comment updated
#                                                          since then, and every review of a pull
#                                                          request updated since then
#
# GH_REPO names the repository (owner/name). Dependabot's text quotes upstream release notes and
# is left as it is.
set -euo pipefail

strip=$(cd "$(dirname "$0")" && pwd)/check-attribution.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cleaned=0
failed=0

# <api-url> <author login> <body file>
clean() {
  local url=${1#https://api.github.com/} author=$2 body=$3 method=PATCH
  [[ $author == 'dependabot[bot]' ]] && return 0
  "$strip" --strip-footer "$body" > "$work/clean"
  cmp -s "$body" "$work/clean" && return 0
  # GitHub refuses an empty body, so a comment that is only a footer stays as it is.
  if ! grep -q '[^[:space:]]' "$work/clean"; then
    echo "left $url: it is only a footer"
    return 0
  fi
  # A review's body is replaced with PUT; a comment's with PATCH.
  [[ $url == */reviews/* ]] && method=PUT
  # One refused write does not stop a sweep; the run still fails at the end.
  if ! gh api -X "$method" "$url" -F "body=@$work/clean" > /dev/null; then
    echo "::error title=attribution::could not remove the footer from $url"
    failed=$((failed + 1))
    return 0
  fi
  echo "removed the footer from $url"
  cleaned=$((cleaned + 1))
}

case "${1:-}" in
  --comment)
    gh api "${2#https://api.github.com/}" > "$work/item.json"
    jq -r '.body // ""' "$work/item.json" > "$work/body"
    clean "$2" "$(jq -r '.user.login // ""' "$work/item.json")" "$work/body"
    ;;
  --since)
    # One JSON object per line: the URL, the author and the body (newlines kept inside JSON).
    item='{url, login: (.user.login // ""), body: (.body // "")} | @json'
    for kind in issues/comments pulls/comments; do
      gh api --paginate "repos/$GH_REPO/$kind?since=$2&per_page=100" --jq ".[] | $item" >> "$work/list"
    done
    # Reviews have no repository-wide listing: read those of each pull request updated since then.
    gh api --paginate "repos/$GH_REPO/issues?state=all&since=$2&per_page=100" \
      --jq '.[] | select(.pull_request) | .number' > "$work/pulls"
    while IFS= read -r pr; do
      gh api --paginate "repos/$GH_REPO/pulls/$pr/reviews?per_page=100" \
        --jq ".[] | .url = \"repos/$GH_REPO/pulls/$pr/reviews/\\(.id)\" | $item" >> "$work/list"
    done < "$work/pulls"
    while IFS= read -r line; do
      jq -r .body <<<"$line" > "$work/body"
      clean "$(jq -r .url <<<"$line")" "$(jq -r .login <<<"$line")" "$work/body"
    done < "$work/list"
    ;;
  *)
    sed -n '2,14p' "$0" >&2
    exit 2
    ;;
esac

echo "strip-comment-footers: $cleaned cleaned, $failed failed"
((failed == 0))
