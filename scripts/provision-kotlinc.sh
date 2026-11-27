#!/usr/bin/env bash
# Provision one Kotlin release's compiler distribution under a cache directory and print its
# launcher. This is the only writer of that cache: `just kotlinc` and the test harness both call it.
#
# A distribution counts only when its whole tested payload is present: the compiler jar, the
# standard library jar, and `build.txt` (the list is `KOTLINC_PAYLOAD` in src/toolchain.rs, which
# readers check with the same rule). Anything less, such as a directory an interrupted run left
# half-written, is replaced. Provisioning takes a per-release lock, downloads and unpacks into a
# staging directory beside the destination, validates the payload there, and publishes the whole
# tree with one rename, so a reader never finds a distribution that is missing part of its payload.
#
#   scripts/provision-kotlinc.sh <kotlin-version> <release-dir> [archive-url]
set -euo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 <kotlin-version> <release-dir> [archive-url]" >&2
  exit 2
fi
ver="$1"
dest="$2"
url="${3:-https://github.com/JetBrains/kotlin/releases/download/v${ver}/kotlin-compiler-${ver}.zip}"
payload=(lib/kotlin-compiler.jar lib/kotlin-stdlib.jar build.txt)

complete() {
  local file
  for file in "${payload[@]}"; do
    [ -f "$1/kotlinc/$file" ] || return 1
  done
}

# What this script returns is the launcher, so it also has to run: a cache with the whole server
# payload but no executable launcher is repaired rather than handed back.
usable() {
  complete "$1" && [ -x "$1/kotlinc/bin/kotlinc" ]
}

if usable "$dest"; then
  echo "$dest/kotlinc/bin/kotlinc"
  exit 0
fi

mkdir -p "$dest"
if [ "${KRUSTY_KOTLINC_LOCKED:-}" != "$dest" ]; then
  # Re-run under an exclusive lock on the release directory. `flock` is not on every host, so the
  # fallback takes the same advisory lock through Python. Python opens descriptors non-inheritable,
  # so the descriptor is made inheritable first: the lock belongs to the open file description,
  # and it is released only when the last descriptor on it closes, which `execv` would otherwise
  # do immediately.
  export KRUSTY_KOTLINC_LOCKED="$dest"
  if command -v flock >/dev/null 2>&1; then
    exec flock "$dest/.provision.lock" "$0" "$@"
  fi
  exec python3 -c '
import fcntl, os, sys
fd = os.open(sys.argv[1], os.O_CREAT | os.O_RDWR)
os.set_inheritable(fd, True)
fcntl.flock(fd, fcntl.LOCK_EX)
os.execv(sys.argv[2], sys.argv[2:])
' "$dest/.provision.lock" "$0" "$@"
fi

# Another process may have finished while this one waited for the lock.
if usable "$dest"; then
  echo "$dest/kotlinc/bin/kotlinc"
  exit 0
fi

# Under the lock no other provisioner is staging, so a `.staging-*` directory is one a killed run
# left behind (the trap below never ran).
rm -rf "$dest"/.staging-*

echo "downloading kotlin-compiler ${ver}…" >&2
staging="$dest/.staging-$$"
trap 'rm -rf "$staging"' EXIT
mkdir -p "$staging"
# GitHub's release CDN answers 504 for minutes at a time. curl doubles its wait from one second,
# so eight retries span about four minutes; a connection that never opens fails within 30s.
curl -fsSL --connect-timeout 30 --retry 8 --retry-max-time 300 "$url" -o "$staging/kotlinc.zip" \
  || { echo "failed to download $url" >&2; exit 1; }
mkdir "$staging/tree"
if command -v unzip >/dev/null 2>&1; then
  unzip -q "$staging/kotlinc.zip" -d "$staging/tree"
elif command -v python3 >/dev/null 2>&1; then
  python3 -c "import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" \
    "$staging/kotlinc.zip" "$staging/tree"
else
  (cd "$staging/tree" && jar xf "$staging/kotlinc.zip")
fi
if ! complete "$staging/tree" || [ ! -f "$staging/tree/kotlinc/bin/kotlinc" ]; then
  echo "the kotlinc ${ver} archive at $url lacks part of its payload (${payload[*]}, bin/kotlinc)" >&2
  exit 1
fi
chmod +x "$staging/tree/kotlinc/bin/kotlinc"
# Only this script, under the lock, replaces the directory; readers pass only a complete one.
rm -rf "$dest/kotlinc"
mv "$staging/tree/kotlinc" "$dest/kotlinc"
echo "$dest/kotlinc/bin/kotlinc"
