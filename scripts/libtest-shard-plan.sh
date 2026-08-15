#!/usr/bin/env bash
# Read `libtest --list --format terse` output on stdin and assign whole top-level modules to shards.
# Output: SHARD_INDEX<TAB>TEST_COUNT<TAB>MODULE, one module per line.
set -euo pipefail
source "$(dirname "$0")/libtest-shards.sh"

shards="${1:-}"
libtest_require_positive_shard_count "$shards" "$0"

LC_ALL=C awk '
  /: test$/ {
    name = $0
    sub(/: test$/, "", name)
    parts = split(name, path, "::")
    if (parts < 2 || path[1] !~ /^[A-Za-z_][A-Za-z0-9_]*$/) {
      printf "libtest-shard-plan: unsupported test name: %s\n", name > "/dev/stderr"
      invalid = 1
      next
    }
    counts[path[1]]++
    found = 1
  }
  END {
    if (invalid || !found) exit 3
    for (module in counts) printf "%d\t%s\n", counts[module], module
  }
' \
  | LC_ALL=C sort -t $'\t' -k1,1nr -k2,2 \
  | awk -F '\t' -v shards="$shards" '
      BEGIN {
        for (i = 0; i < shards; i++) load[i] = 0
      }
      {
        lightest = 0
        for (i = 1; i < shards; i++) {
          if (load[i] < load[lightest]) lightest = i
        }
        load[lightest] += $1
        printf "%d\t%d\t%s\n", lightest, $1, $2
      }
    '
