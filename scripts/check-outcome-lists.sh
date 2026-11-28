#!/usr/bin/env bash
# The platform/version outcome manifests only shrink: a change may remove entries, never add one, so a
# regression cannot be recorded as expected and a fix cannot pay for one. That covers the box
# expected failures and not-applicable lists, the byte-identity non-identical lists, the CLI corpus failures, the diagnostics
# false accepts, and uncovered KLIB implementations. A release records a manifest only where it differs from its predecessor's, and a
# release with none reads the newest one below it, so what is compared is the manifest each release
# READS: deleting a release's own file is an addition when the inherited one lists more. A release
# the base did not support yet (a newly supported Kotlin version, or a new target) is exempt, and so
# is one the head's kotlin-versions manifest no longer supports: it reads no inventory at all. Only
# the head's own changes count: it is compared with its merge base, so a branch that is behind a
# base which has since dropped entries is not charged with them. The CLI not-applicable lists and the
# diagnostics not-scored lists are not checked here: they record what the reference kotlinc cannot
# run in this environment, and the required reference run proves every entry instead.
# A byte-identity list also accepts a row for a case the change itself adds: a new owned regression
# program, or a case the base's JVM lane did not apply (see new_byte_identity_keys_removed). Every
# key the base scored, upstream or owned, only shrinks, and a renamed owned program is not new.
# The other exception to "only shrink" is a plugin corpus's `included` inventory
# (src/plugins/<name>/boxes/included): it lists the cases that must pass, so it only GROWS. A change
# may add entries, never remove one, including by deleting a release's own file when the inherited
# one lists fewer; otherwise a case that starts failing could be dropped from the list instead of
# fixed. A release the base did not support yet is exempt, as everywhere else.
# The sole approved #2459 migration pins each exact loaded old/new blob and effective release in
# outcome-list-migrations.tsv. Only a row introduced in this comparison can authorize that reset;
# the approval is spent once the merge-base or actual target records it. A different blob transition
# cannot renew it, and a recorded row cannot cover even one additional failure.
#
#   scripts/check-outcome-lists.sh <base-rev> <head-rev>
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
order_versions() {
  "$script_dir/sort-kotlin-versions.sh" | uniq
}

if [ "$#" -ne 2 ]; then
  sed -n '2,14p' "$0" >&2
  exit 2
fi
target=$(git rev-parse --verify "$1^{commit}")
head=$(git rev-parse --verify "$2^{commit}")
base=$(git merge-base "$target" "$head")

entries() {
  git show "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' | grep -v -e '^$' -e '^#' | sort -u || true
}

migration_file=scripts/outcome-list-migrations.tsv
migration_entries() {
  local revision=$1 object contents
  if ! object=$(git ls-tree "$revision" -- "$migration_file" 2>/dev/null | awk '{ print $3 }'); then
    echo "outcome-lists: cannot read committed migration records at $revision:$migration_file" >&2
    return 1
  fi
  [ -n "$object" ] || return 0
  if ! contents=$(git cat-file blob "$object" 2>/dev/null); then
    echo "outcome-lists: cannot read committed migration records at $revision:$migration_file" >&2
    return 1
  fi
  printf '%s\n' "$contents" |
    sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e '/^$/d' -e '/^#/d' | sort -u
}

base_migrations=$(migration_entries "$base")
target_migrations=$(migration_entries "$target")
head_migrations=$(migration_entries "$head")
removed_migrations=$(comm -23 <(printf '%s\n' "$base_migrations") <(printf '%s\n' "$head_migrations"))
if [ -n "$removed_migrations" ]; then
  echo "outcome-lists: approved migration records are append-only; removed rows:" >&2
  sed 's/^/    /' <<<"$removed_migrations" >&2
  exit 1
fi

# The releases a revision supports: its kotlin-versions manifest, or, without one, the releases its
# lane records.
releases() {
  local rev=$1 lane=$2
  if git cat-file -e "$rev:kotlin-versions" 2>/dev/null; then
    git show "$rev:kotlin-versions" | sed 's/#.*//' | awk 'NF { print $1 }'
  else
    recorded "$rev" "$lane"
  fi | order_versions
}

recorded() {
  git ls-tree --name-only "$1" -- "$2/" 2>/dev/null |
    sed -nE 's#.*/([0-9]+\.[0-9]+\.[0-9]+(-Beta[1-9][0-9]*|-RC[1-9][0-9]*|-RC)?)\.txt$#\1#p' | order_versions
}

# The manifest `release` reads at a revision: its own, else the newest recorded supported release
# below it. Prints nothing when none applies.
reads() {
  local rev=$1 lane=$2 release=$3 candidates
  candidates=$(recorded "$rev" "$lane" | grep -Fx -f <(releases "$rev" "$lane") || true)
  [ -n "$candidates" ] || return 0
  { echo "$candidates"; echo "$release"; } | order_versions | sed "/^${release//./\\.}\$/q" |
    grep -Fx -f <(echo "$candidates") | tail -n 1 || true
}

approved_migration() {
  local lane=$1 release=$2 from=$3 to=$4 old_blob new_blob row
  case "$lane" in
    tests/box_expected_failures/native-klib|tests/box_expected_failures/wasm-js-klib|tests/box_expected_failures/wasm-wasi-klib) ;;
    *) return 1 ;;
  esac
  [ -n "$to" ] || return 1
  # The approval covers one batch of exact transitions, not future rows with the same number.
  if printf '%s\n' "$base_migrations" "$target_migrations" | awk -F '\t' '$5 == "2459" { spent = 1 } END { exit !spent }'; then
    return 1
  fi
  old_blob=$(git rev-parse "$base:$lane/$from.txt")
  new_blob=$(git rev-parse "$head:$lane/$to.txt")
  row=$(printf '%s\t%s\t%s\t%s\t2459' "$lane" "$release" "$old_blob" "$new_blob")
  printf '%s\n' "$head_migrations" | grep -Fx -- "$row" >/dev/null || return 1
  if printf '%s\n' "$base_migrations" | grep -Fx -- "$row" >/dev/null; then
    return 1
  fi
}

# The owned regression programs the head renamed or moved from the base: a moved file is a case the
# base already scored under another name, so it is not new.
renamed_owned_regressions() {
  git diff -M --diff-filter=R --name-only "$base" "$head" -- tests/box_regressions
}

# A byte-identity row may arrive with the case it describes: an owned regression program that the
# head adds (a `regressions/<path>` key whose tests/box_regressions/<path> file is absent at the
# base, present at the head, and not a rename of a base file), or a case the base's JVM lane did
# not apply (listed in the base's not-applicable manifest). A key the base already scored only leaves
# the list. That includes an upstream case under the `regressions/` prefix, which has no owned
# file at all, and an owned program that was only renamed. Prints the added keys that remain
# charged.
new_byte_identity_keys_removed() {
  local release=$1 added=$2 not_applicable_from key owned renamed
  not_applicable_from=$(reads "$base" tests/box_expected_not_applicable/jvm "$release")
  renamed=$(renamed_owned_regressions)
  while IFS= read -r key; do
    [ -n "$key" ] || continue
    case "$key" in
      regressions/*)
        owned="tests/box_regressions/${key#regressions/}"
        if ! git cat-file -e "$base:$owned" 2>/dev/null &&
          git cat-file -e "$head:$owned" 2>/dev/null &&
          ! grep -Fx -- "$owned" <<<"$renamed" >/dev/null; then
          continue
        fi
        ;;
    esac
    if [ -n "$not_applicable_from" ] &&
      entries "$base:tests/box_expected_not_applicable/jvm/$not_applicable_from.txt" | grep -Fx -- "$key" >/dev/null; then
      continue
    fi
    printf '%s\n' "$key"
  done <<<"$added"
}

added_total=0
removed_total=0
while read -r lane; do
  # An `included` inventory only grows; every other manifest only shrinks.
  case "$lane" in
    src/plugins/*/boxes/included/*) grows=1 ;;
    *) grows=0 ;;
  esac
  base_releases=$(releases "$base" "$lane")
  head_releases=$(releases "$head" "$lane")
  for release in $( { echo "$base_releases"; releases "$head" "$lane"; recorded "$head" "$lane"; } | sed '/^$/d' | order_versions); do
    grep -qx -- "$release" <<<"$base_releases" || continue
    if git cat-file -e "$head:kotlin-versions" 2>/dev/null && ! grep -qx -- "$release" <<<"$head_releases"; then
      continue
    fi
    from=$(reads "$base" "$lane" "$release")
    [ -n "$from" ] || continue
    to=$(reads "$head" "$lane" "$release")
    if [ "$grows" = 1 ]; then
      if [ -n "$to" ]; then
        added=$(comm -23 <(entries "$base:$lane/$from.txt") <(entries "$head:$lane/$to.txt"))
      else
        added=$(entries "$base:$lane/$from.txt")
      fi
    elif [ -n "$to" ]; then
      added=$(comm -13 <(entries "$base:$lane/$from.txt") <(entries "$head:$lane/$to.txt"))
    else
      added=""
    fi
    if [ "$lane" = tests/box_byte_non_identical/jvm ] && [ -n "$added" ]; then
      added=$(new_byte_identity_keys_removed "$release" "$added")
    fi
    [ -n "$added" ] || continue
    count=$(wc -l <<<"$added" | tr -d '[:space:]')
    manifest="$lane/$release.txt"
    [ "$to" = "$release" ] || manifest="$manifest (reads ${to:+$lane/$to.txt})"
    if [ "$grows" = 1 ]; then
      removed_total=$((removed_total + count))
      echo "outcome-lists: $manifest loses $count entr$([ "$count" -eq 1 ] && echo y || echo ies):" >&2
      sed 's/^/    /' <<<"$added" >&2
      continue
    fi
    if approved_migration "$lane" "$release" "$from" "$to"; then
      echo "outcome-lists: authorized #2459 migration for $manifest: $count added entr$([ "$count" -eq 1 ] && echo y || echo ies)" >&2
      continue
    fi
    added_total=$((added_total + count))
    echo "outcome-lists: $manifest gains $count entr$([ "$count" -eq 1 ] && echo y || echo ies):" >&2
    sed 's/^/    /' <<<"$added" >&2
  done
done < <(
  { git ls-tree -r --name-only "$base"; git ls-tree -r --name-only "$head"; } |
    grep -E '^(tests/(box_byte_non_identical|box_expected_failures|box_expected_not_applicable|cli_expected_failures|diagnostics_false_accepts|diagnostics_ledger_mismatches|klib_uncovered_implementations)|src/plugins/[^/]+/boxes/(expected_not_applicable|included))/[^/]+/[^/]+\.txt$' |
    sed 's|/[^/]*$||' | sort -u
)

if ((removed_total)); then
  echo "outcome-lists: a plugin corpus's included cases only grow; fix the failing cases above instead of dropping them" >&2
fi
if ((added_total)); then
  echo "outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them" >&2
fi
if ((added_total || removed_total)); then
  exit 1
fi
