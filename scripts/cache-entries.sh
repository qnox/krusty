#!/usr/bin/env bash
# Print one digest naming the set of entries under a cache directory (empty or missing: the digest
# of nothing). Entries are named by their own content identity and never rewritten, so a changed
# digest means a run added an entry that a cache save would publish.
set -euo pipefail
dir="$1"
if [ -d "$dir" ]; then
    (cd "$dir" && find . -type f ! -name '.*.tmp' -print | LC_ALL=C sort)
fi | sha256sum | cut -c1-64
