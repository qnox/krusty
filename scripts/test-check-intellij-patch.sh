#!/usr/bin/env bash
# Offline contract of scripts/check-intellij-patch.sh: a source file is accepted only when it matches
# its pinned SHA-256, whether it came from the cache or from a download.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0
fail() { echo "FAIL: $*" >&2; failed=$((failed + 1)); }

revision=$(sed -n 's/^revision=//p' "$repo/scripts/check-intellij-patch.sh")
mkdir -p "$work/bin" "$work/tools/intellij-community/$revision/build/jvm-rules"
printf 'not the pinned file\n' > "$work/tools/intellij-community/$revision/build/jvm-rules/MODULE.bazel"

# A download that answers with the wrong content; each call is recorded.
cat > "$work/bin/curl" <<STUB
#!/bin/sh
echo call >> "$work/curl-calls"
while [ \$# -gt 0 ]; do
  [ "\$1" = -o ] && out="\$2"
  shift
done
printf 'wrong content\n' > "\$out"
STUB
chmod +x "$work/bin/curl"

if msg=$(PATH="$work/bin:$PATH" KRUSTY_TOOLS_DIR="$work/tools" "$repo/scripts/check-intellij-patch.sh" 2>&1); then
  fail "a file with the wrong content was accepted"
else
  case "$msg" in
    *"build/jvm-rules/MODULE.bazel at $revision does not match its pinned SHA-256"*) ;;
    *) fail "unexpected refusal: $msg" ;;
  esac
fi
[ "$(wc -l < "$work/curl-calls")" -eq 1 ] || fail "a cached file with the wrong content was not fetched again"
[ -z "$(find "$work/tools" -name '*.part.*')" ] || fail "a partial download was left behind"

[ "$failed" -eq 0 ] || { echo "check-intellij-patch: $failed failed assertions" >&2; exit 1; }
echo "check-intellij-patch: ok"
