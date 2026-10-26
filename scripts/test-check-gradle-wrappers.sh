#!/usr/bin/env bash
# Exercise scripts/check-gradle-wrappers.sh in a fixture Git checkout whose curl on PATH answers
# services.gradle.org from fixed checksums, so each case asserts the exact verdict offline.
set -euo pipefail

# Hooks export repository-local settings that override even `git -C`. Never let fixture commands
# inherit the calling repository's object store, worktree, or index.
while IFS= read -r variable; do
  unset "$variable"
done < <(git rev-parse --local-env-vars)

repo=$(cd "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
tree=$fixture/tree
mkdir -p "$tree/scripts" "$fixture/bin"
cp "$repo/scripts/check-gradle-wrappers.sh" "$tree/scripts/"
git init -q "$tree"

printf 'wrapper jar\n' > "$fixture/wrapper.jar"
jar_sum=$(sha256sum "$fixture/wrapper.jar" | cut -d' ' -f1)
zip_sum=1111111111111111111111111111111111111111111111111111111111111111
cat > "$fixture/bin/curl" <<CURL
#!/usr/bin/env bash
case "\${@: -1}" in
  https://services.gradle.org/distributions/gradle-8.14.3-wrapper.jar.sha256) echo $jar_sum ;;
  https://services.gradle.org/distributions/gradle-8.14.3-bin.zip.sha256) echo $zip_sum ;;
  *) echo "unexpected curl call: \$*" >&2; exit 22 ;;
esac
CURL
chmod +x "$fixture/bin/curl"

# One wrapper at $1, from the fixture jar, with the given distributionUrl and distributionSha256Sum.
wrapper() {
  local dir=$tree/$1/gradle/wrapper
  mkdir -p "$dir"
  cp "$fixture/wrapper.jar" "$dir/gradle-wrapper.jar"
  {
    echo 'distributionBase=GRADLE_USER_HOME'
    echo "distributionSha256Sum=$3"
    echo "distributionUrl=$2"
  } > "$dir/gradle-wrapper.properties"
  git -C "$tree" add "$1"
}
good_url='https\://services.gradle.org/distributions/gradle-8.14.3-bin.zip'

failures=0
run_case() {
  local name=$1 status=$2 stderr=$3 actual_status=0
  PATH="$fixture/bin:$PATH" "$tree/scripts/check-gradle-wrappers.sh" > "$fixture/out" 2> "$fixture/err" ||
    actual_status=$?
  if [[ $actual_status != "$status" || -s "$fixture/out" || $(cat "$fixture/err") != "$stderr" ]]; then
    echo "$name: expected status $status and stderr:" >&2
    printf '%s\n' "$stderr" >&2
    echo "got status $actual_status, stdout:" >&2
    cat "$fixture/out" >&2
    echo "stderr:" >&2
    cat "$fixture/err" >&2
    failures=$((failures + 1))
  fi
}

run_case 'no wrapper' 1 'no committed gradle-wrapper.jar'

wrapper a "$good_url" "$zip_sum"
wrapper b "$good_url" "$zip_sum"
run_case 'published wrappers' 0 ''

echo tampered >> "$tree/b/gradle/wrapper/gradle-wrapper.jar"
tampered=$(sha256sum "$tree/b/gradle/wrapper/gradle-wrapper.jar" | cut -d' ' -f1)
run_case 'tampered jar' 1 \
  "b/gradle/wrapper/gradle-wrapper.jar: SHA-256 $tampered is not Gradle 8.14.3's wrapper jar ($jar_sum)"

wrapper b "$good_url" 2222222222222222222222222222222222222222222222222222222222222222
run_case 'wrong distribution checksum' 1 \
  "b/gradle/wrapper/gradle-wrapper.properties: distributionSha256Sum '2222222222222222222222222222222222222222222222222222222222222222' is not gradle-8.14.3-bin.zip's SHA-256 ($zip_sum)"

wrapper b "$good_url" ''
run_case 'unpinned distribution' 1 \
  "b/gradle/wrapper/gradle-wrapper.properties: distributionSha256Sum '' is not gradle-8.14.3-bin.zip's SHA-256 ($zip_sum)"

wrapper b 'https\://example.com/gradle-8.14.3-bin.zip' "$zip_sum"
run_case 'foreign distribution' 1 \
  'b/gradle/wrapper/gradle-wrapper.properties: distributionUrl is not a services.gradle.org distribution: https://example.com/gradle-8.14.3-bin.zip'

wrapper b "$good_url" "$zip_sum"
rm "$tree/b/gradle/wrapper/gradle-wrapper.properties"
run_case 'wrapper without properties' 1 \
  'b/gradle/wrapper/gradle-wrapper.jar: not a wrapper jar beside its gradle-wrapper.properties'

if ((failures)); then
  exit 1
fi
echo "check-gradle-wrappers: all cases passed"
