#!/usr/bin/env bash
# Drop kotlinc reference inventories the suite no longer reads, before CI saves the cache.
#
#   scripts/prune-reference-cache.sh <cache dir> <max age in days>
#
# Each entry of target/cache/ref-classes is a directory named by its input fingerprint and
# published with an OK or REJECTED marker. FAILED is the previous rejection marker: nothing reads
# it, and it ages out like any other unread entry. tests/box_reference_classes.rs refreshes the
# marker's modification time on every read. An entry whose marker is older than the age limit
# belongs to a case, input or salt no recent run looked up, so it goes. A directory without a marker is an
# abandoned staging tree from a cut-short publish and goes too. A run that stopped early leaves
# the entries it never reached alone: they are dropped only once they stay unread for the whole
# age limit.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <cache dir> <max age in days>" >&2
  exit 2
fi
cache=$1
days=$2
case "$days" in
  '' | *[!0-9]* | 0)
    echo "max age must be a positive whole number of days: $days" >&2
    exit 2
    ;;
esac
[ -d "$cache" ] || exit 0

kept=0
dropped=0
for entry in "$cache"/*/; do
  [ -d "$entry" ] || continue
  entry=${entry%/}
  marker=
  for name in OK REJECTED FAILED; do
    if [ -f "$entry/$name" ]; then
      marker=$entry/$name
      break
    fi
  done
  if [ -n "$marker" ] && [ -z "$(find "$marker" -mmin "+$((days * 1440))" -print)" ]; then
    kept=$((kept + 1))
  else
    rm -rf -- "$entry"
    dropped=$((dropped + 1))
  fi
done
echo "reference cache: kept $kept entries, dropped $dropped unread for ${days}+ days or unpublished"
