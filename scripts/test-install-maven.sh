#!/usr/bin/env bash
# Offline contract of scripts/install-maven.sh: an installed copy is reused without a download and
# joins the workflow PATH, and a version without a pinned checksum is refused.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0
fail() { echo "FAIL: $*" >&2; failed=$((failed + 1)); }

mkdir -p "$work/tools/maven/apache-maven-3.9.11/bin"
printf '#!/bin/sh\n' > "$work/tools/maven/apache-maven-3.9.11/bin/mvn"
chmod +x "$work/tools/maven/apache-maven-3.9.11/bin/mvn"

# Reuse: curl is shadowed by a failing stub, so any download attempt fails the run.
mkdir "$work/bin"
printf '#!/bin/sh\necho "unexpected download" >&2\nexit 1\n' > "$work/bin/curl"
chmod +x "$work/bin/curl"
out=$(PATH="$work/bin:$PATH" KRUSTY_TOOLS_DIR="$work/tools" GITHUB_PATH="$work/path" \
  "$repo/scripts/install-maven.sh" 3.9.11) || fail "an installed Maven was not reused"
[ "$out" = "$work/tools/maven/apache-maven-3.9.11" ] || fail "printed home was '$out'"
[ "$(cat "$work/path")" = "$work/tools/maven/apache-maven-3.9.11/bin" ] || fail "GITHUB_PATH was not extended"

if msg=$(KRUSTY_TOOLS_DIR="$work/tools" "$repo/scripts/install-maven.sh" 9.9.9 2>&1); then
  fail "an unpinned version was accepted"
else
  case "$msg" in
    *"no pinned checksum for Maven 9.9.9"*) ;;
    *) fail "unexpected refusal: $msg" ;;
  esac
fi

[ "$failed" -eq 0 ] || { echo "install-maven: $failed failed assertions" >&2; exit 1; }
echo "install-maven: ok"
