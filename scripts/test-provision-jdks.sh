#!/usr/bin/env bash
# Offline test of scripts/provision-jdks.sh: stub `curl` stands in for Adoptium and its CDN. A
# download whose size differs from the published one must fail naming both sizes.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"
cat > "$work/bin/curl" <<'STUB'
#!/usr/bin/env bash
out=""; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
if [[ "$url" == https://api.adoptium.net/* ]]; then
  echo '[{"binary":{"package":{"link":"https://cdn.example/jdk.tar.gz","checksum":"00","size":1000}}}]'
else
  printf '<?xml version="1.0"?><BlockList/>' > "$out"
fi
STUB
chmod +x "$work/bin/curl"
status=0
output="$(PATH="$work/bin:$PATH" KRUSTY_JDK_CACHE="$work/jdks" KRUSTY_JDK_8_HOME=/nonexistent JAVA_HOME= \
  bash "$root/scripts/provision-jdks.sh" 99 2>&1)" || status=$?
expected='provision-jdks: downloading Temurin 99
provision-jdks: Temurin 99 download is 33 bytes, expected 1000 (begins: <?xml version="1.0"?><BlockList/>)'
[ "$status" -ne 0 ] || { echo "expected a failure" >&2; exit 1; }
[ "$output" = "$expected" ] || { printf 'unexpected output:\n%s\n' "$output" >&2; exit 1; }
echo "provision-jdks rejects a download of the wrong size"
