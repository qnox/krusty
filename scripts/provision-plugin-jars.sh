#!/usr/bin/env bash
# Fetch every Maven jar the plugin corpora name, for one Kotlin version, into the dependency cache.
#
#   scripts/provision-plugin-jars.sh [kotlin-version]     default: `just max-version`
#
# A corpus descriptor (src/plugins/<name>/boxes/corpus.txt) names its reference plugin jar and its
# runtime jars as artifacts: `maven:`, `google:` and `maven-kotlin:` ones come from a repository.
# The test process fetches a missing one lazily, but under a shared 60-second budget a slow
# transfer exhausts it and every later case fails with "needs the ... compiler plugin jar". This
# step fetches them once, with retries, before any test runs, and fails loudly when it cannot.
# Files land where `toolchain::ensure_maven` looks: `$KRUSTY_DEPS_CACHE/<artifact>-<version>.jar`,
# `$HOME/.cache/krusty-deps` when it is unset. Export the same value for this script and the tests.
set -euo pipefail
cd "$(dirname "$0")/.."

version="${1:-$(just max-version)}"
# The same default `toolchain::ensure_repository_artifact` reads, so a bare run primes what the tests use.
cache="${KRUSTY_DEPS_CACHE:-$HOME/.cache/krusty-deps}"
dist_lib="$PWD/target/cache/kotlinc/$version/kotlinc/lib"
mkdir -p "$cache"

fetch() { # repository group artifact version
  local file="$cache/$3-$4.jar" url
  [ -f "$file" ] && return 0
  url="$1/${2//.//}/$3/$4/$3-$4.jar"
  curl -sfL --retry 5 --retry-all-errors --retry-delay 2 --connect-timeout 10 --max-time 120 \
    -o "$file.download-$$" "$url" || { rm -f "$file.download-$$"; echo "cannot fetch $url" >&2; return 1; }
  mv "$file.download-$$" "$file"
}

status=0
while IFS= read -r line; do
  key="${line%%=*}"; key="${key// /}"
  case "$key" in plugin-jar|runtime) ;; *) continue ;; esac
  value="${line#*=}"
  # `dist:<jar> | maven:<group>:<artifact>:<version>`: the distribution's jar wins when it ships it.
  dist=""
  IFS='|' read -ra alternatives <<<"$value"
  for alternative in "${alternatives[@]}"; do
    alternative="${alternative#"${alternative%%[![:space:]]*}"}"; alternative="${alternative%"${alternative##*[![:space:]]}"}"
    IFS=: read -r kind a b c <<<"$alternative"
    case "$kind" in
      dist) dist="$a"; [ -f "$dist_lib/$a" ] && break ;;
      maven) fetch https://repo1.maven.org/maven2 "$a" "$b" "$c" || status=1 ;;
      google) fetch https://dl.google.com/android/maven2 "$a" "$b" "$c" || status=1 ;;
      maven-kotlin) fetch https://repo1.maven.org/maven2 "$a" "$b" "$version" || status=1 ;;
    esac
  done
done < <(cat src/plugins/*/boxes/corpus.txt)
exit "$status"
