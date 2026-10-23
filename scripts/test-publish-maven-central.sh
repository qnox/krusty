#!/usr/bin/env bash
# Exercise scripts/publish-maven-central.sh against a stand-in Gradle build, Central Portal, and
# clock: a copy of the script runs in a fixture Git checkout with a bare remote, its gradlew writes
# a release repository, and curl, date, and sleep on PATH answer from per-case files.
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
mkdir -p "$tree/scripts" "$tree/tools/krusty-gradle" "$fixture/bin"
cp "$repo/scripts/publish-maven-central.sh" "$tree/scripts/"
git init -q --bare "$fixture/remote.git"
git init -q "$tree"
git -C "$tree" remote add origin "$fixture/remote.git"
# Some Git configurations negotiate pushes, which a fresh bare remote answers with a warning.
git -C "$tree" config push.negotiate false
ref=refs/krusty/central/1.2.3

# gradlew publishes the implementation and marker poms (and the implementation jar), signed unless
# $fixture/unsigned exists, plus the maven-metadata files the bundle must leave out.
cat > "$tree/tools/krusty-gradle/gradlew" <<GRADLEW
#!/usr/bin/env bash
set -euo pipefail
echo "\$*" >> "$fixture/builds"
root="$tree/tools/krusty-gradle/build/release-repository/dev/krustythecompiler"
rm -rf "$tree/tools/krusty-gradle/build"
for f in krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.jar \
  krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.pom \
  dev.krustythecompiler.gradle.plugin/1.2.3/dev.krustythecompiler.gradle.plugin-1.2.3.pom; do
  mkdir -p "\$root/\$(dirname "\$f")"
  echo "\$f" > "\$root/\$f"
  echo md5 > "\$root/\$f.md5"
  [[ -e "$fixture/unsigned" ]] || echo sig > "\$root/\$f.asc"
done
echo metadata > "\$root/krusty-gradle-plugin/maven-metadata.xml"
echo md5 > "\$root/krusty-gradle-plugin/maven-metadata.xml.md5"
echo "Gradle output"
GRADLEW

# curl logs each call, answers the published check for artifact NAME from $fixture/published-NAME,
# each upload with the next deployment id (recording the bundle's entries), and each status call
# with the next line of $fixture/states-ID, repeating the last one. $fixture/curl-fails makes every
# call fail as a request deadline does.
cat > "$fixture/bin/curl" <<CURL
#!/usr/bin/env bash
set -euo pipefail
echo "\$*" >> "$fixture/curl.log"
if [[ -e "$fixture/curl-fails" ]]; then
  echo "curl: (28) Operation timed out after 60000 milliseconds" >&2
  exit 28
fi
for arg in "\$@"; do
  case \$arg in
    bundle=@*) unzip -Z1 "\${arg#bundle=@}" | sort > "$fixture/bundle" ;;
    */published\?*)
      name=\${arg#*&name=}
      printf '{"published":%s}' "\$(cat "$fixture/published-\${name%%&*}")"
      exit 0 ;;
    */upload\?*)
      n=\$((\$(cat "$fixture/uploads" 2>/dev/null || echo 0) + 1))
      echo "\$n" > "$fixture/uploads"
      printf "dep-\$n"
      exit 0 ;;
    */status\?id=*)
      states="$fixture/states-\${arg#*id=}"
      state=\$(head -n 1 "\$states")
      [[ \$(wc -l < "\$states") -le 1 ]] || sed -i 1d "\$states"
      printf '{"deploymentState":"%s","errors":{"pom":["missing scm"]}}' "\$state"
      exit 0 ;;
  esac
done
echo "unexpected curl call: \$*" >&2
exit 1
CURL
# The clock moves only when the script sleeps.
cat > "$fixture/bin/date" <<DATE
#!/usr/bin/env bash
cat "$fixture/clock"
DATE
cat > "$fixture/bin/sleep" <<SLEEP
#!/usr/bin/env bash
echo \$((\$(cat "$fixture/clock") + \$1)) > "$fixture/clock"
SLEEP
chmod +x "$tree/tools/krusty-gradle/gradlew" "$fixture/bin/"*

failures=0
fail() {
  echo "$1" >&2
  failures=$((failures + 1))
}
run_case() {
  local name=$1 status=$2 stdout=$3 stderr=$4 actual_status=0
  PATH="$fixture/bin:$PATH" MAVEN_CENTRAL_USERNAME=user MAVEN_CENTRAL_PASSWORD=token \
    ORG_GRADLE_PROJECT_signingInMemoryKey=key \
    "$tree/scripts/publish-maven-central.sh" 1.2.3 > "$fixture/out" 2> "$fixture/err" ||
    actual_status=$?
  if [[ $actual_status != "$status" || $(cat "$fixture/out") != "$stdout" || $(cat "$fixture/err") != "$stderr" ]]; then
    fail "$name: expected status $status, stdout '$stdout', stderr '$stderr'
$name: got status $actual_status, stdout '$(cat "$fixture/out")', stderr '$(cat "$fixture/err")'"
  fi
}
check() {
  [[ $2 == "$3" ]] || fail "$1: expected '$3', got '$2'"
}
count() {
  if [[ -e $1 ]]; then wc -l < "$1"; else echo 0; fi
}
recorded() {
  git -C "$fixture/remote.git" log -1 --format=%s "$ref" 2>/dev/null || echo none
}
# A new release: nothing published, no recorded deployment, a fresh clock and call log.
reset() {
  rm -f "$fixture"/{builds,bundle,unsigned,curl-fails,curl.log,states-*}
  git -C "$fixture/remote.git" update-ref -d "$ref" 2>/dev/null || true
  echo false > "$fixture/published-krusty-gradle-plugin"
  echo false > "$fixture/published-dev.krustythecompiler.gradle.plugin"
  echo 1000 > "$fixture/clock"
}
# A rerun of the same release: the recorded deployment and Central's state carry over.
rerun() {
  rm -f "$fixture"/{builds,bundle,curl.log}
}
gradle_tasks="--no-daemon -p $tree/tools/krusty-gradle -PkrustyPluginVersion=1.2.3 clean publishAllPublicationsToReleaseRepository"

reset
echo true > "$fixture/published-krusty-gradle-plugin"
echo true > "$fixture/published-dev.krustythecompiler.gradle.plugin"
run_case published 0 'krusty-gradle-plugin 1.2.3 is already on Maven Central' ''
check 'published: builds' "$(count "$fixture/builds")" 0
check 'published: requests' "$(cat "$fixture/curl.log")" "$(printf '%s\n' \
  "-fsS --connect-timeout 10 --max-time 60 -H Authorization: Bearer $(printf user:token | base64 -w0) https://central.sonatype.com/api/v1/publisher/published?namespace=dev.krustythecompiler&name=krusty-gradle-plugin&version=1.2.3" \
  "-fsS --connect-timeout 10 --max-time 60 -H Authorization: Bearer $(printf user:token | base64 -w0) https://central.sonatype.com/api/v1/publisher/published?namespace=dev.krustythecompiler&name=dev.krustythecompiler.gradle.plugin&version=1.2.3")"

reset
echo true > "$fixture/published-dev.krustythecompiler.gradle.plugin"
run_case partial 1 '' 'only one of the implementation (false) and marker (true) of 1.2.3 is on Maven Central'
check 'partial: builds' "$(count "$fixture/builds")" 0

# A first run: the upload is recorded, followed through validation, and accepted.
reset
echo 0 > "$fixture/uploads"
printf 'VALIDATING\nPUBLISHING\n' > "$fixture/states-dep-1"
run_case accepted 0 $'uploaded deployment dep-1\ndeployment dep-1 is PUBLISHING' 'Gradle output'
check 'accepted: builds' "$(cat "$fixture/builds")" "$gradle_tasks"
check 'accepted: recorded' "$(recorded)" 'central deployment dep-1 of krusty-gradle-plugin 1.2.3'
check 'accepted: clock' "$(cat "$fixture/clock")" 1010
check 'accepted: upload' "$(grep -c -- '--max-time 120 .* -F bundle=@.*/upload?name=krusty-gradle-plugin-1.2.3&publishingType=AUTOMATIC$' "$fixture/curl.log")" 1
check 'accepted: every request has a deadline' "$(grep -vc -- '--connect-timeout 10 --max-time ' "$fixture/curl.log")" 0
check 'accepted: bundle' "$(cat "$fixture/bundle")" "$(for f in \
  dev.krustythecompiler.gradle.plugin/1.2.3/dev.krustythecompiler.gradle.plugin-1.2.3.pom \
  krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.jar \
  krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.pom; do
  printf 'dev/krustythecompiler/%s\n' "$f" "$f.asc" "$f.md5"
done)"

# The Plugin Portal step then fails while Central is still PUBLISHING, and the release is rerun:
# the recorded deployment is followed, not uploaded again.
rerun
run_case 'rerun while publishing' 0 $'following recorded deployment dep-1\ndeployment dep-1 is PUBLISHING' ''
check 'rerun while publishing: builds' "$(count "$fixture/builds")" 0
check 'rerun while publishing: uploads' "$(cat "$fixture/uploads")" 1

# Once both publications are visible, a further rerun skips Central altogether.
rerun
echo PUBLISHED > "$fixture/states-dep-1"
echo true > "$fixture/published-krusty-gradle-plugin"
echo true > "$fixture/published-dev.krustythecompiler.gradle.plugin"
run_case 'rerun after publishing' 0 'krusty-gradle-plugin 1.2.3 is already on Maven Central' ''
check 'rerun after publishing: builds' "$(count "$fixture/builds")" 0

# A deployment that failed validation does not hold the version; the rerun uploads again.
reset
echo 0 > "$fixture/uploads"
echo FAILED > "$fixture/states-dep-1"
run_case failed 1 'uploaded deployment dep-1' $'Gradle output\ndeployment dep-1 failed:\n{\n  "pom": [\n    "missing scm"\n  ]\n}'
rerun
echo PUBLISHING > "$fixture/states-dep-2"
run_case 'rerun after failure' 0 $'recorded deployment dep-1 failed; uploading again\nuploaded deployment dep-2\ndeployment dep-2 is PUBLISHING' 'Gradle output'
check 'rerun after failure: recorded' "$(recorded)" 'central deployment dep-2 of krusty-gradle-plugin 1.2.3'

# A deployment that never settles is given up on after five minutes of fake clock.
reset
echo 0 > "$fixture/uploads"
echo VALIDATING > "$fixture/states-dep-1"
run_case 'still validating' 1 'uploaded deployment dep-1' $'Gradle output\ndeployment dep-1 is still VALIDATING after 300s'
check 'still validating: clock' "$(cat "$fixture/clock")" 1300
check 'still validating: status calls' "$(grep -c 'status?id=dep-1' "$fixture/curl.log")" 31

# A request that hits its deadline fails the step.
reset
touch "$fixture/curl-fails"
run_case 'request deadline' 28 '' 'curl: (28) Operation timed out after 60000 milliseconds'
check 'request deadline: builds' "$(count "$fixture/builds")" 0

reset
echo 0 > "$fixture/uploads"
touch "$fixture/unsigned"
run_case unsigned 1 '' $'Gradle output\nunsigned artifacts:\n./dev/krustythecompiler/dev.krustythecompiler.gradle.plugin/1.2.3/dev.krustythecompiler.gradle.plugin-1.2.3.pom\n./dev/krustythecompiler/krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.jar\n./dev/krustythecompiler/krusty-gradle-plugin/1.2.3/krusty-gradle-plugin-1.2.3.pom'
check 'unsigned: uploads' "$(cat "$fixture/uploads")" 0
check 'unsigned: recorded' "$(recorded)" none

if [[ $failures -ne 0 ]]; then
  echo "$failures Maven Central publishing check(s) failed" >&2
  exit 1
fi
echo "Maven Central publishing: 10 cases passed"
