#!/usr/bin/env bash
# Enforce the authorship rule in AGENTS.md: no AI, assistant or tool attribution in commit
# identities, commit messages, or pull request text.
#
#   scripts/check-attribution.sh <rev-range>        check every commit in the range (CI)
#   scripts/check-attribution.sh --message <file>   check a commit message and the identity the
#                                                   commit is about to get (commit-msg hook)
#   scripts/check-attribution.sh --text <file>      check free text such as a PR title and body
set -euo pipefail

# Assistant and vendor names, their no-reply addresses, and the usual attribution phrases.
pattern='claude|codex|anthropic|openai|copilot|chatgpt|gemini|cursor(agent| agent)|\bgpt-?[0-9]|generated (with|by)|🤖'

failures=0

report() {
  echo "attribution: $1" >&2
  failures=$((failures + 1))
}

check_text() {
  local what=$1 text=$2 hits
  hits=$(grep -inE "$pattern" <<<"$text" || true)
  [[ -z $hits ]] || report "$what mentions an AI tool:"$'\n'"$(sed 's/^/    /' <<<"$hits")"
}

case "${1:-}" in
  --message)
    check_text "commit message" "$(grep -v '^#' "$2")"
    check_text "author" "$(git var GIT_AUTHOR_IDENT)"
    check_text "committer" "$(git var GIT_COMMITTER_IDENT)"
    ;;
  --text)
    check_text "${3:-text}" "$(cat "$2")"
    ;;
  "" | -*)
    sed -n '2,8p' "$0" >&2
    exit 2
    ;;
  *)
    while read -r sha; do
      check_text "commit $sha identity" "$(git log -1 --format='%an <%ae> / %cn <%ce>' "$sha")"
      check_text "commit $sha message" "$(git log -1 --format='%B' "$sha")"
    done < <(git rev-list "$1")
    ;;
esac

if ((failures)); then
  echo "attribution: see \"Authorship and attribution\" in AGENTS.md" >&2
  exit 1
fi
