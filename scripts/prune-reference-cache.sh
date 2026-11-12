#!/usr/bin/env bash
# Drop kotlinc reference inventories the suite no longer reads, before CI saves the cache.
#
#   scripts/prune-reference-cache.sh <cache dir> <max age in days> [<read-since file>]
#
# Each entry of target/cache/ref-classes is a directory named by its input fingerprint and
# published with an OK or FAILED marker; tests/box_reference_classes.rs refreshes the marker's
# modification time on every read. An entry whose marker is older than the age limit belongs to a
# case, input or salt no recent run looked up, so it goes. A directory without a marker is an
# abandoned staging tree from a cut-short publish and goes too. A run that stopped early leaves
# the entries it never reached alone: they are dropped only once they stay unread for the whole
# age limit.
#
# With a read-since file, an entry whose marker is not newer than that file goes as well. A CI row
# that seeded its cache from another row's touches the file after the restore, so it keeps only the
# entries its own run read or compiled.
set -euo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 <cache dir> <max age in days> [<read-since file>]" >&2
  exit 2
fi
cache=$1
days=$2
since=${3:-}
case "$days" in
  '' | *[!0-9]* | 0)
    echo "max age must be a positive whole number of days: $days" >&2
    exit 2
    ;;
esac
if [ -n "$since" ] && [ ! -f "$since" ]; then
  echo "read-since file not found: $since" >&2
  exit 2
fi
[ -d "$cache" ] || exit 0

kept=0
dropped=0
for entry in "$cache"/*/; do
  [ -d "$entry" ] || continue
  entry=${entry%/}
  marker=
  for name in OK FAILED; do
    if [ -f "$entry/$name" ]; then
      marker=$entry/$name
      break
    fi
  done
  if [ -n "$marker" ] && [ -z "$(find "$marker" -mmin "+$((days * 1440))" -print)" ] \
    && { [ -z "$since" ] || [ "$marker" -nt "$since" ]; }; then
    kept=$((kept + 1))
  else
    rm -rf -- "$entry"
    dropped=$((dropped + 1))
  fi
done
if [ -n "$since" ]; then
  echo "reference cache: kept $kept entries, dropped $dropped unread for ${days}+ days, unread since $since, or unpublished"
else
  echo "reference cache: kept $kept entries, dropped $dropped unread for ${days}+ days or unpublished"
fi
