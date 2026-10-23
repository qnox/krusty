#!/usr/bin/env bash
# Exercise scripts/publish-maven-central.sh against a stand-in Gradle build and Central Portal: a
# copy of the script runs in a fixture tree whose gradlew writes a release repository, and a curl
# on PATH answers the Portal's published, upload, and status calls from per-case files.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/tree/scripts" "$fixture/tree/tools/krusty-gradle" "$fixture/bin"
cp "$repo/scripts/publish-maven-central.sh" "$fixture/tree/scripts/"

# gradlew publishes one artifact and its pom, signed unless $fixture/unsigned exists, plus the
# maven-metadata files the bundle must leave out.
cat > "$fixture/tree/tools/krusty-gradle/gradlew" <<GRADLEW
#!/usr/bin/env bash
set -euo pipefail
echo "\$*" > "$fixture/gradlew.args"
dir="$fixture/tree/tools/krusty-gradle/build/release-repository/dev/krustythecompiler/krusty-gradle-plugin"
rm -rf "$fixture/tree/tools/krusty-gradle/build"
mkdir -p "\$dir/1.2.3"
for f in krusty-gradle-plugin-1.2.3.jar krusty-gradle-plugin-1.2.3.pom; do
  echo "\$f" > "\$dir/1.2.3/\$f"
  echo md5 > "\$dir/1.2.3/\$f.md5"
  [[ -e "$fixture/unsigned" ]] || echo sig > "\$dir/1.2.3/\$f.asc"
done
echo metadata > "\$dir/maven-metadata.xml"
echo md5 > "\$dir/maven-metadata.xml.md5"
GRADLEW

# curl answers the published check from $fixture/published, the upload with deployment id dep-1
# (recording the bundle's entries), and each status call with the next line of $fixture/states.
cat > "$fixture/bin/curl" <<CURL
#!/usr/bin/env bash
set -euo pipefail
for arg in "\$@"; do
  case \$arg in
    Authorization:*) echo "\$arg" > "$fixture/auth" ;;
    bundle=@*) unzip -Z1 "\${arg#bundle=@}" | sort > "$fixture/bundle" ;;
    */published\?*) cat "$fixture/published"; exit 0 ;;
    */upload\?*) echo "\$arg" > "$fixture/upload.url"; printf dep-1; exit 0 ;;
    */status\?id=dep-1)
      state=\$(head -n 1 "$fixture/states")
      sed -i 1d "$fixture/states"
      printf '{"deploymentState":"%s","errors":{"pom":["missing scm"]}}' "\$state"
      exit 0 ;;
  esac
done
echo "unexpected curl call: \$*" >&2
exit 1
CURL
chmod +x "$fixture/tree/tools/krusty-gradle/gradlew" "$fixture/bin/curl"

failures=0
run_case() {
  local name=$1 status=$2 stdout=$3 stderr=$4 actual_status=0
  PATH="$fixture/bin:$PATH" MAVEN_CENTRAL_USERNAME=user MAVEN_CENTRAL_PASSWORD=token \
    ORG_GRADLE_PROJECT_signingInMemoryKey=key \
    "$fixture/tree/scripts/publish-maven-central.sh" 1.2.3 > "$fixture/out" 2> "$fixture/err" ||
    actual_status=$?
  if [[ $actual_status != "$status" || $(cat "$fixture/out") != "$stdout" || $(cat "$fixture/err") != "$stderr" ]]; then
    echo "$name: expected status $status, stdout '$stdout', stderr '$stderr'" >&2
    echo "$name: got status $actual_status, stdout '$(cat "$fixture/out")', stderr '$(cat "$fixture/err")'" >&2
    failures=$((failures + 1))
  fi
}
check() {
  if [[ $2 != "$3" ]]; then
    echo "$1: expected '$3', got '$2'" >&2
    failures=$((failures + 1))
  fi
}
reset() {
  rm -f "$fixture"/{gradlew.args,bundle,upload.url,unsigned,auth}
  echo '{"published":false}' > "$fixture/published"
}

reset
echo '{"published":true}' > "$fixture/published"
run_case published 0 'dev.krustythecompiler:krusty-gradle-plugin:1.2.3 is already on Maven Central' ''
check 'published: no build' "$([[ -e $fixture/gradlew.args ]] && echo built || echo none)" none
check 'published: bearer token' "$(cat "$fixture/auth")" "Authorization: Bearer $(printf user:token | base64 -w0)"

reset
echo PUBLISHING > "$fixture/states"
run_case accepted 0 $'uploaded deployment dep-1\ndeployment dep-1 is PUBLISHING' ''
check 'accepted: gradle tasks' "$(cat "$fixture/gradlew.args")" \
  "--no-daemon -p $fixture/tree/tools/krusty-gradle -PkrustyPluginVersion=1.2.3 clean publishAllPublicationsToReleaseRepository"
check 'accepted: upload' "$(cat "$fixture/upload.url")" \
  'https://central.sonatype.com/api/v1/publisher/upload?name=krusty-gradle-plugin-1.2.3&publishingType=AUTOMATIC'
check 'accepted: bundle' "$(cat "$fixture/bundle")" "$(printf '%s\n' \
  dev/krustythecompiler/krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.jar \
  dev/krustythecompiler/krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.jar.asc \
  dev/krustythecompiler/krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.jar.md5 \
  dev/krustythecompiler/krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.pom \
  dev/krustythecompiler/krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.pom.asc \
  dev/krustythecompiler/krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.pom.md5)"

reset
echo FAILED > "$fixture/states"
run_case failed 1 'uploaded deployment dep-1' $'deployment dep-1 failed:\n{\n  "pom": [\n    "missing scm"\n  ]\n}'

reset
touch "$fixture/unsigned"
run_case unsigned 1 '' $'unsigned artifacts:\n./dev/krustythecompiler/krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.jar\n./dev/krustythecompiler/krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.pom'
check 'unsigned: no upload' "$([[ -e $fixture/upload.url ]] && echo uploaded || echo none)" none

if [[ $failures -ne 0 ]]; then
  echo "$failures Maven Central publishing check(s) failed" >&2
  exit 1
fi
echo "Maven Central publishing: 4 cases passed"
