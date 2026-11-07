#!/usr/bin/env bash
# Execute the public-key publisher with real disposable signatures and a stand-in keyserver.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'gpgconf --homedir "$fixture/home" --kill gpg-agent 2>/dev/null || true; rm -rf "$fixture"' EXIT
mkdir -m 700 "$fixture/home"
mkdir "$fixture/bin" "$fixture/dist"
gpg_() { gpg --homedir "$fixture/home" --batch --quiet --pinentry-mode loopback --passphrase fixture "$@"; }
gpg_ --quick-gen-key 'Release fixture <fixture@example.invalid>' ed25519 cert never 2>/dev/null
root=$(gpg_ --with-colons --list-keys | awk -F: '$1 == "fpr" { print $10; exit }')
gpg_ --quick-add-key "$root" ed25519 sign never
signer=$(gpg_ --with-colons --list-keys | awk -F: '$1 == "fpr" { fpr=$10 } END { print fpr }')
version=2.4.21-build.7
sums=$fixture/dist/krusty-$version-SHA256SUMS
certificate=$fixture/dist/krusty-$version-signing-key.asc
echo '0000  compiler.tar.gz' > "$sums"
gpg_ --armor --detach-sign --local-user "$signer!" --output "$sums.asc" "$sums"
gpg_ --armor --export "$root" > "$certificate"
cp "$certificate" "$fixture/root.asc"

cat > "$fixture/bin/curl" <<'CURL'
#!/usr/bin/env bash
set -euo pipefail
output= payload= url=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --output) output=$2; shift 2 ;;
    --write-out|--connect-timeout|--max-time) shift 2 ;;
    --data-urlencode) payload=${2#keytext@}; shift 2 ;;
    --silent|--show-error|--fail) shift ;;
    https://*) url=$1; shift ;;
    *) exit 90 ;;
  esac
done
if [[ -n $payload ]]; then
  [[ $url == https://keyserver.ubuntu.com/pks/add ]]
  echo POST >> "$FIXTURE/requests"
  cp "$payload" "$FIXTURE/uploaded.asc"
  : > "$output"
  exit 0
fi
[[ $url == "https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x$SIGNER" ]]
echo GET >> "$FIXTURE/requests"
case "$MODE" in
  unavailable) exit 28 ;;
  http-error) : > "$output"; printf 503; exit 0 ;;
  corrupt) printf 'invalid certificate' > "$output"; printf 200; exit 0 ;;
  missing|delayed)
    if [[ ! -f $FIXTURE/uploaded.asc ]]; then : > "$output"; printf 404; exit 0; fi
    if [[ $MODE == delayed && ! -f $FIXTURE/slept ]]; then : > "$output"; printf 404; exit 0; fi ;;
  never) : > "$output"; printf 404; exit 0 ;;
esac
cp "$FIXTURE/public.asc" "$output"
printf 200
CURL
cat > "$fixture/bin/sleep" <<'SLEEP'
#!/usr/bin/env bash
[[ $1 == 10 ]]
echo sleep >> "$FIXTURE/sleeps"
: > "$FIXTURE/slept"
SLEEP
chmod +x "$fixture/bin/curl" "$fixture/bin/sleep"
cp "$certificate" "$fixture/public.asc"
export FIXTURE=$fixture SIGNER=$signer

run() {
  local name=$1 expected=$2 requests=$3 actual=0
  rm -f "$fixture/requests" "$fixture/uploaded.asc" "$fixture/slept" "$fixture/sleeps"
  MODE=$name PATH="$fixture/bin:$PATH" KRUSTY_RELEASE_ROOT_KEY="$fixture/root.asc" \
    bash "$repo/scripts/publish-release-signing-key.sh" "$version" "$fixture/dist" \
    > "$fixture/out" 2> "$fixture/err" || actual=$?
  if [[ $actual != "$expected" || $(cat "$fixture/requests" 2>/dev/null || true) != "$requests" ]]; then
    echo "$name: expected exit $expected and requests '$requests'; got exit $actual" >&2
    cat "$fixture/out" "$fixture/err" >&2
    exit 1
  fi
  if [[ -f $fixture/uploaded.asc ]]; then
    cmp "$fixture/public.asc" "$fixture/uploaded.asc"
  fi
}
run present 0 GET
run missing 0 $'GET\nPOST\nGET'
run delayed 0 $'GET\nPOST\nGET\nGET'
[[ $(cat "$fixture/sleeps") == sleep ]]
run never 1 $'GET\nPOST\nGET\nGET\nGET\nGET\nGET\nGET'
[[ $(wc -l < "$fixture/sleeps") == 5 ]]
run unavailable 1 GET
run http-error 1 GET
run corrupt 1 GET
echo tampered >> "$sums"
run invalid-signature 1 ''
gpg_ --quick-gen-key 'Other root <other@example.invalid>' ed25519 cert never 2>/dev/null
gpg_ --armor --export 'Other root' > "$fixture/root.asc"
run unpinned-root 1 ''
echo 'release public-key publication: 9 cases passed'
