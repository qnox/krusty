#!/usr/bin/env bash
# Offline test of scripts/fetch-conformance-deps.sh: a stub `curl` stands in for Maven Central.
set -euo pipefail
export FETCH_RETRY_WAIT=0
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/src"
cp "$root/src/toolchain.rs" "$work/src/toolchain.rs"
runtime="$(sed -n 's/^pub const ATOMICFU_RUNTIME_VERSION: &str = "\(.*\)";$/\1/p' "$root/src/toolchain.rs")"
[ -n "$runtime" ] || { echo "no ATOMICFU_RUNTIME_VERSION in src/toolchain.rs" >&2; exit 1; }
serialization="$(sed -n 's/^pub const SERIALIZATION_VERSION: &str = "\(.*\)";$/\1/p' "$root/src/toolchain.rs")"
compose="$(sed -n 's/^pub const COMPOSE_RUNTIME_VERSION: &str = "\(.*\)";$/\1/p' "$root/src/toolchain.rs")"

# The stub records each URL and writes the file named by `-o`; `FAIL_URL_PART` makes a URL answer 404.
cat > "$work/bin/curl" <<'STUB'
#!/usr/bin/env bash
out=""; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    --retry|--retry-delay) shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
echo "$url" >> "$STUB_LOG"
if [ -n "${FAIL_URL_PART:-}" ] && [[ "$url" == *"$FAIL_URL_PART"* ]]; then
  echo "curl: (22) The requested URL returned error: 404" >&2
  exit 22
fi
echo "bytes of $url" > "$out"
STUB
chmod +x "$work/bin/curl"
export PATH="$work/bin:$PATH" STUB_LOG="$work/log"
: > "$STUB_LOG"
deps="$work/deps"

SCRIPT_ROOT="$work" KRUSTY_DEPS_CACHE="$deps" bash "$root/scripts/fetch-conformance-deps.sh" 2.4.20

expected=$(printf '%s\n' \
  "atomicfu-jvm-$runtime.jar" \
  "atomicfu-linuxx64-$runtime.klib" \
  "atomicfu-wasm-js-$runtime.klib" \
  "atomicfu-wasm-wasi-$runtime.klib" \
  "collection-jvm-1.5.0.jar" \
  "kotlin-atomicfu-compiler-plugin-embeddable-2.4.20.jar" \
  "kotlinx-coroutines-core-jvm-1.9.0.jar" \
  "kotlinx-serialization-core-jvm-$serialization.jar" \
  "kotlinx-serialization-json-jvm-$serialization.jar" \
  "runtime-annotation-jvm-$compose.jar" \
  "runtime-desktop-$compose.jar")
[ "$(ls "$deps")" = "$expected" ] || { echo "unexpected files:"; ls "$deps"; exit 1; }
[ "$(wc -l < "$STUB_LOG")" -eq 11 ] || { echo "expected 11 downloads"; cat "$STUB_LOG"; exit 1; }
grep -qx "https://dl.google.com/android/maven2/androidx/compose/runtime/runtime-desktop/$compose/runtime-desktop-$compose.jar" "$STUB_LOG"
grep -qx "https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-atomicfu-compiler-plugin-embeddable/2.4.20/kotlin-atomicfu-compiler-plugin-embeddable-2.4.20.jar" "$STUB_LOG"

# A second run finds everything in place and downloads nothing.
: > "$STUB_LOG"
SCRIPT_ROOT="$work" KRUSTY_DEPS_CACHE="$deps" bash "$root/scripts/fetch-conformance-deps.sh" 2.4.20
[ ! -s "$STUB_LOG" ] || { echo "a complete cache was downloaded again"; cat "$STUB_LOG"; exit 1; }

# Another Kotlin version adds only its own plugin.
SCRIPT_ROOT="$work" KRUSTY_DEPS_CACHE="$deps" bash "$root/scripts/fetch-conformance-deps.sh" 2.5.0-RC
[ "$(wc -l < "$STUB_LOG")" -eq 1 ] && grep -q 'embeddable/2.5.0-RC/' "$STUB_LOG"

# A refused download fails naming the URL and leaves no partial file.
rm -rf "$deps"
if FAIL_URL_PART=atomicfu-wasm-js SCRIPT_ROOT="$work" KRUSTY_DEPS_CACHE="$deps" \
  bash "$root/scripts/fetch-conformance-deps.sh" 2.4.20 2> "$work/err"; then
  echo "a refused download did not fail"; exit 1
fi
grep -q "atomicfu-wasm-js/$runtime/atomicfu-wasm-js-$runtime.klib" "$work/err"
grep -q 'error: 404' "$work/err"
! ls "$deps" | grep -q 'part'
echo "fetch-conformance-deps: ok"
