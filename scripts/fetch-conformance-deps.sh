#!/usr/bin/env bash
# Download the extra libraries the conformance rows of one Kotlin version, and the e2e suite, read from
# the dependency cache (`KRUSTY_DEPS_CACHE`, default target/cache/krusty-deps): the atomicfu compiler plugin of that
# exact version and the pinned atomicfu runtime for the JVM, Native and Wasm targets. The names and
# the runtime version are the ones src/toolchain.rs resolves, so a row that restores this cache never
# downloads them. The pinned serialization, Compose runtime and coroutines jars the e2e suite
# provisions come with them. A file already in place is kept; a refused download is retried after a wait
# (Central answers 429 under load) and then fails naming the URL.
set -euo pipefail

version="${1:?usage: fetch-conformance-deps.sh <kotlin-version>}"
root="${SCRIPT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
cache="${KRUSTY_DEPS_CACHE:-$PWD/target/cache/krusty-deps}"
central=https://repo1.maven.org/maven2
google=https://dl.google.com/android/maven2

runtime="$(sed -n 's/^pub const ATOMICFU_RUNTIME_VERSION: &str = "\(.*\)";$/\1/p' "$root/src/toolchain.rs")"
[ -n "$runtime" ] || { echo "no ATOMICFU_RUNTIME_VERSION in src/toolchain.rs" >&2; exit 1; }

const() { # name of a pinned `pub const NAME: &str = "value";` in src/toolchain.rs
  local value
  value="$(sed -n "s/^pub const $1: &str = \"\(.*\)\";$/\1/p" "$root/src/toolchain.rs")"
  [ -n "$value" ] || { echo "no $1 in src/toolchain.rs" >&2; exit 1; }
  echo "$value"
}
serialization="$(const SERIALIZATION_VERSION)"
compose="$(const COMPOSE_RUNTIME_VERSION)"

mkdir -p "$cache"

fetch() { # group artifact version extension [repository]
  local file="$cache/$2-$3.$4"
  [ -f "$file" ] && return 0
  local url="${5:-$central}/${1//./\/}/$2/$3/$2-$3.$4"
  local part="$file.part.$$"
  trap 'rm -f "$part"' RETURN
  local attempt
  for attempt in 1 2 3 4 5; do
    if curl -sSfL -o "$part" "$url" 2> "$part.err"; then
      mv "$part" "$file"
      rm -f "$part.err"
      return 0
    fi
    echo "attempt $attempt of $url: $(tr '\n' ' ' < "$part.err")" >&2
    sleep "${FETCH_RETRY_WAIT:-$((attempt * 10))}"
  done
  rm -f "$part" "$part.err"
  echo "error: $url was not downloaded after 5 attempts" >&2
  return 1
}

fetch org.jetbrains.kotlin kotlin-atomicfu-compiler-plugin-embeddable "$version" jar
fetch org.jetbrains.kotlinx atomicfu-jvm "$runtime" jar
for klib in atomicfu-linuxx64 atomicfu-wasm-js atomicfu-wasm-wasi; do
  fetch org.jetbrains.kotlinx "$klib" "$runtime" klib
done

fetch org.jetbrains.kotlinx kotlinx-serialization-core-jvm "$serialization" jar
fetch org.jetbrains.kotlinx kotlinx-serialization-json-jvm "$serialization" jar
fetch org.jetbrains.kotlinx kotlinx-coroutines-core-jvm 1.9.0 jar
fetch androidx.compose.runtime runtime-desktop "$compose" jar "$google"
fetch androidx.compose.runtime runtime-annotation-jvm "$compose" jar "$google"
fetch androidx.collection collection-jvm 1.5.0 jar "$google"
