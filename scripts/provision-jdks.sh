#!/usr/bin/env bash
# Print `KRUSTY_JDK_<n>_HOME=<home>` for each requested JDK feature release, downloading the
# Eclipse Temurin build into target/cache/jdks/<n> when no installed home provides it.
#
#   scripts/provision-jdks.sh 8 11 17 21 25
#
# An installed home is found where the CLI corpus test looks (kotlin_cli_jvm_conformance:
# `KRUSTY_JDK_<n>_HOME`, `JAVA_HOME_<n>_X64`/`_ARM64`, `JAVA_HOME`, `/usr/lib/jvm`), by the
# `release` file each home carries. A download is checked against the sha256 Adoptium publishes for
# it, and its size must match the one Adoptium publishes; a missing or mismatching one fails.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
cache="${KRUSTY_JDK_CACHE:-$repo_root/target/cache/jdks}"

case "$(uname -m)" in
  x86_64 | amd64) arch=x64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) echo "provision-jdks: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac
case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=mac ;;
  *) echo "provision-jdks: unsupported system: $(uname -s)" >&2; exit 1 ;;
esac

is_release() { # <home> <n>
  [ -f "$1/release" ] && grep -qE "^JAVA_VERSION=\"($2|1\\.$2)[.\"+_-]" "$1/release"
}

installed_home() { # <n>
  local n=$1 var home
  for var in "KRUSTY_JDK_${n}_HOME" "JAVA_HOME_${n}_X64" "JAVA_HOME_${n}_ARM64" JAVA_HOME; do
    home="${!var:-}"
    if [ -n "$home" ] && is_release "$home" "$n"; then echo "$home"; return 0; fi
  done
  for home in /usr/lib/jvm/*/ /Library/Java/JavaVirtualMachines/*/Contents/Home/; do
    if [ -d "$home" ] && is_release "${home%/}" "$n"; then echo "${home%/}"; return 0; fi
  done
  return 1
}

download() { # <n> <dest>
  local n=$1 dest=$2 meta link sum work
  meta="$(curl -fsSL "https://api.adoptium.net/v3/assets/latest/$n/hotspot?architecture=$arch&image_type=jdk&os=$os&vendor=eclipse")"
  link="$(python3 -I -c 'import json,sys; b=json.load(sys.stdin)[0]["binary"]["package"]; print(b["link"])' <<<"$meta")"
  sum="$(python3 -I -c 'import json,sys; b=json.load(sys.stdin)[0]["binary"]["package"]; print(b["checksum"])' <<<"$meta")"
  size="$(python3 -I -c 'import json,sys; b=json.load(sys.stdin)[0]["binary"]["package"]; print(b["size"])' <<<"$meta")"
  [ -n "$link" ] && [ -n "$sum" ] && [ -n "$size" ] || { echo "provision-jdks: no Temurin $n build for $os/$arch" >&2; return 1; }
  work="$(mktemp -d "$cache.XXXXXX")"
  echo "provision-jdks: downloading Temurin $n" >&2
  curl -fsSL "$link" -o "$work/jdk.tar.gz"
  # A proxy or CDN can answer 200 with a small error document; name it before the checksum does.
  got="$(wc -c < "$work/jdk.tar.gz" | tr -d ' ')"
  [ "$got" = "$size" ] \
    || { echo "provision-jdks: Temurin $n download is $got bytes, expected $size (begins: $(head -c 60 "$work/jdk.tar.gz" | tr -c '[:print:]' ' '))" >&2; rm -rf "$work"; return 1; }
  [ "$( (sha256sum "$work/jdk.tar.gz" 2>/dev/null || shasum -a 256 "$work/jdk.tar.gz") | cut -d' ' -f1)" = "$sum" ] \
    || { echo "provision-jdks: checksum mismatch for Temurin $n" >&2; rm -rf "$work"; return 1; }
  mkdir "$work/home"
  tar -xzf "$work/jdk.tar.gz" -C "$work/home" --strip-components=1
  # A macOS archive keeps the home under Contents/Home.
  [ -d "$work/home/Contents/Home" ] && { mv "$work/home" "$work/mac" && mv "$work/mac/Contents/Home" "$work/home"; }
  is_release "$work/home" "$n" || { echo "provision-jdks: the Temurin $n download is not JDK $n" >&2; rm -rf "$work"; return 1; }
  publish "$n" "$work/home" "$dest"
  rm -rf "$work"
}

# Publish a staged home at <dest> under a per-release lock, keeping a complete home another caller
# published first: that home may be in use, so it is never replaced. Only an incomplete leftover is.
publish() { # <n> <staged home> <dest>
  local n=$1 staged=$2 dest=$3 lock="$3.lock" waited=0
  until mkdir "$lock" 2>/dev/null; do
    sleep 1
    waited=$((waited + 1))
    [ "$waited" -le 300 ] || { echo "provision-jdks: timed out waiting for $lock" >&2; return 1; }
  done
  if ! is_release "$dest" "$n"; then
    rm -rf "$dest"
    mv "$staged" "$dest" || { rmdir "$lock"; return 1; }
  fi
  rmdir "$lock"
}

mkdir -p "$cache"
for n in "$@"; do
  if home="$(installed_home "$n")"; then
    echo "KRUSTY_JDK_${n}_HOME=$home"
    continue
  fi
  dest="$cache/$n"
  is_release "$dest" "$n" || download "$n" "$dest"
  echo "KRUSTY_JDK_${n}_HOME=$dest"
done
