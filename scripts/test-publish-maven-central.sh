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

export GIT_AUTHOR_NAME='Fixture Caller'
export GIT_AUTHOR_EMAIL='caller@example.invalid'
export GIT_COMMITTER_NAME='Fixture Caller'
export GIT_COMMITTER_EMAIL='caller@example.invalid'

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
GIT_AUTHOR_NAME='Source Author' GIT_AUTHOR_EMAIL=source@example.invalid \
  GIT_COMMITTER_NAME='Source Integrator' GIT_COMMITTER_EMAIL=integrator@example.invalid \
  git -C "$tree" commit --allow-empty -qm 'Publication source'
# The checkout and inherited environment must not replace the selected release author.
source_commit=$(git -C "$tree" rev-parse HEAD)
git -C "$tree" commit --allow-empty -qm 'Unrelated checkout'
export RELEASE_SHA=$source_commit
ref=refs/krusty/central/1.2.3

# gradlew publishes the implementation and marker poms (and the implementation jar), signed unless
# $fixture/unsigned exists, plus the maven-metadata files the bundle must leave out.
cat > "$tree/tools/krusty-gradle/gradlew" <<GRADLEW
#!/usr/bin/env bash
set -euo pipefail
echo "\$*" >> "$fixture/builds"
env | grep '^MAVEN_CENTRAL_' >> "$fixture/gradle-env" || true
if [[ -e "$fixture/build-fails" ]]; then echo "Gradle failed" >&2; exit 7; fi
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
# with the next line of $fixture/states-ID, repeating the last one. A status call takes the seconds
# in $fixture/status-seconds (none by default) on the fake clock, and one longer than its --max-time
# times out after that limit, as curl does. $fixture/curl-fails makes every call time out.
cat > "$fixture/bin/curl" <<CURL
#!/usr/bin/env python3
import json
import pathlib
import subprocess
import sys
import urllib.parse

root = pathlib.Path("$fixture")
original = sys.argv[1:]
args = []
writeout = ""
i = 0
while i < len(original):
    if original[i] == "--write-out":
        writeout = original[i + 1]
        i += 2
        continue
    arg = original[i]
    if arg.startswith("@"):
        source = pathlib.Path(arg[1:])
        (root / "auth-header").write_bytes(source.read_bytes())
        (root / "auth-mode").write_text(oct(source.stat().st_mode & 0o777)[2:] + chr(10))
        arg = "@authorization-file"
    args.append(arg)
    i += 1
with (root / "curl.log").open("a") as log:
    log.write(" ".join(args) + chr(10))


def finish(code, body="", http="200", elapsed=0):
    if code == 0:
        sys.stdout.write(body)
    elif any("S" in arg for arg in args if arg.startswith("-")):
        sys.stderr.write("curl: private-deployment-999 PRIVATE_TRANSPORT_DETAIL" + chr(10))
    rendered = writeout.replace("%{stderr}", "").replace("%{http_code}", http)
    rendered = rendered.replace("%{exitcode}", str(code)).replace("%{time_total}", f"{elapsed:.6f}")
    sys.stderr.write(rendered.replace(chr(92) + "n", chr(10)))
    raise SystemExit(code)


if (root / "curl-fails").exists():
    finish(28, http="000", elapsed=60)
if (root / "http-code").exists():
    finish(22, http=(root / "http-code").read_text().strip())
for arg in args:
    if arg.startswith("bundle=@"):
        entries = subprocess.check_output(["unzip", "-Z1", arg[len("bundle=@"):]], text=True)
        (root / "bundle").write_text(chr(10).join(sorted(entries.splitlines())) + chr(10))
    elif "/published?" in arg:
        name = urllib.parse.parse_qs(urllib.parse.urlsplit(arg).query)["name"][0]
        finish(0, '{"published":' + (root / ("published-" + name)).read_text().strip() + '}')
    elif "/upload?" in arg:
        upload = root / "uploads"
        count = int(upload.read_text()) + 1 if upload.exists() else 1
        upload.write_text(str(count) + chr(10))
        finish(0, "dep-" + str(count))
    elif "/status?id=" in arg:
        limit = int(args[args.index("--max-time") + 1])
        seconds = int((root / "status-seconds").read_text()) if (root / "status-seconds").exists() else 0
        clock = root / "clock"
        clock.write_text(str(int(clock.read_text()) + min(seconds, limit)) + chr(10))
        if seconds > limit:
            finish(28, http="000", elapsed=limit)
        identifier = urllib.parse.parse_qs(urllib.parse.urlsplit(arg).query)["id"][0]
        states = root / ("states-" + identifier)
        lines = states.read_text().splitlines()
        if len(lines) > 1:
            states.write_text(chr(10).join(lines[1:]) + chr(10))
        finish(0, json.dumps({"deploymentState": lines[0], "errors": {"pom": ["PRIVATE_RESPONSE_DETAIL"]}}), elapsed=seconds)
raise SystemExit("unexpected mock request")
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
# Extra environment for the next run_case calls, such as GITHUB_ACTIONS=true.
case_env=()
run_case() {
  local name=$1 status=$2 stdout=$3 stderr=$4 actual_status=0
  env -u GITHUB_ACTIONS "${case_env[@]}" PATH="$fixture/bin:$PATH" MAVEN_CENTRAL_USERNAME=user MAVEN_CENTRAL_PASSWORD=token \
    ORG_GRADLE_PROJECT_signingInMemoryKey=key \
    "$tree/scripts/publish-maven-central.sh" 1.2.3 > "$fixture/out" 2> "$fixture/err" ||
    actual_status=$?
  grep '^\[release\] ' "$fixture/err" > "$fixture/progress" || true
  sed '/^\[release\] /d' "$fixture/err" > "$fixture/messages"
  # Every progress field has a fixed schema; responses, URLs, records and credentials stay out.
  if grep -Ev '^\[release\] stage=[a-z-]+ version=1[.]2[.]3 source=([0-9a-f]{40}|unavailable) elapsed_seconds=[0-9]+([.][0-9]+)? outcome=(start|done|failed|http) exit=[0-9]+( http_status=[0-9]{3})?$' "$fixture/progress"; then
    fail "$name: malformed progress record"
  fi
  if sed '/^::add-mask::/d' "$fixture/out" "$fixture/err" | grep -Eq 'dep-[0-9]|PRIVATE_|user:token|dXNlcjp0b2tlbg=='; then
    fail "$name: private request or response detail leaked"
  fi
  if [[ $actual_status != "$status" || $(cat "$fixture/out") != "$stdout" || $(cat "$fixture/messages") != "$stderr" ]]; then
    fail "$name: expected status $status, stdout '$stdout', stderr '$stderr'
$name: got status $actual_status, stdout '$(cat "$fixture/out")', stderr '$(cat "$fixture/messages")'"
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
  rm -f "$fixture"/{builds,gradle-env,auth-header,auth-mode,bundle,unsigned,curl-fails,http-code,build-fails,curl.log,states-*,status-seconds}
  git -C "$tree" remote set-url origin "$fixture/remote.git"
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
token=$(printf user:token | base64 -w0)
check 'published: header' "$(cat "$fixture/auth-header")" "Authorization: Bearer $token"
check 'published: header file mode' "$(cat "$fixture/auth-mode")" 600
check 'published: token off the command line' "$(grep -c -e "$token" -e user:token "$fixture/curl.log")" 0
check 'published: requests' "$(cat "$fixture/curl.log")" "$(printf '%s\n' \
  "-fs --connect-timeout 10 --max-time 60 -H @authorization-file https://central.sonatype.com/api/v1/publisher/published?namespace=dev.krustythecompiler&name=krusty-gradle-plugin&version=1.2.3" \
  "-fs --connect-timeout 10 --max-time 60 -H @authorization-file https://central.sonatype.com/api/v1/publisher/published?namespace=dev.krustythecompiler&name=dev.krustythecompiler.gradle.plugin&version=1.2.3")"
# Under GitHub Actions the bearer encoding, which GitHub does not know to mask, is masked first.
case_env=(GITHUB_ACTIONS=true)
run_case 'published under Actions' 0 "::add-mask::$token
krusty-gradle-plugin 1.2.3 is already on Maven Central" ''
case_env=()

reset
echo true > "$fixture/published-dev.krustythecompiler.gradle.plugin"
run_case partial 1 '' 'only one of the implementation and marker is on Maven Central'
check 'partial: builds' "$(count "$fixture/builds")" 0

# An invalid source cannot leave an upload that the publisher is unable to record.
reset
echo 0 > "$fixture/uploads"
echo PUBLISHING > "$fixture/states-dep-1"
invalid_source=$(git -C "$tree" rev-parse --verify --end-of-options 'missing-publication-source^{commit}' 2>&1 || true)
case_env=(RELEASE_SHA=missing-publication-source)
run_case 'invalid publication source' 128 '' ''
check 'invalid publication source: builds' "$(count "$fixture/builds")" 0
check 'invalid publication source: uploads' "$(cat "$fixture/uploads")" 0
check 'invalid publication source: recorded' "$(recorded)" none
case_env=()

# A first run: the upload is recorded, followed through validation, and accepted.
reset
echo 0 > "$fixture/uploads"
printf 'VALIDATING\nPUBLISHING\n' > "$fixture/states-dep-1"
run_case accepted 0 $'uploaded deployment\ndeployment is PUBLISHING' 'Gradle output'
progress_line() {
  printf '[release] stage=%s version=1.2.3 source=%s elapsed_seconds=%s outcome=%s exit=%s%s\n' \
    "$1" "$source_commit" "$2" "$3" "$4" "${5:-}"
}
check 'accepted: exact stage progress' "$(cat "$fixture/progress")" "$(
  progress_line availability 0 start 0
  progress_line availability 0.000000 http 0 ' http_status=200'
  progress_line availability 0.000000 http 0 ' http_status=200'
  progress_line availability 0 done 0
  progress_line journal-read 0 start 0
  progress_line journal-read 0 done 0
  progress_line source-check 0 start 0
  progress_line source-check 0 done 0
  progress_line publication 0 start 0
  progress_line gradle-build 0 start 0
  progress_line gradle-build 0 done 0
  progress_line bundle 0 start 0
  progress_line bundle 0 done 0
  progress_line upload 0 start 0
  progress_line upload 0.000000 http 0 ' http_status=200'
  progress_line upload 0 done 0
  progress_line publication 0 done 0
  progress_line journal-write 0 start 0
  progress_line journal-write 0 done 0
  progress_line validation 0 start 0
  progress_line validation 0.000000 http 0 ' http_status=200'
  progress_line validation 0.000000 http 0 ' http_status=200'
  progress_line validation 10 done 0
)"
check 'accepted: builds' "$(cat "$fixture/builds")" "$gradle_tasks"
check 'accepted: Portal token kept from the build' "$(count "$fixture/gradle-env")" 0
check 'accepted: recorded' "$(recorded)" 'central deployment dep-1 of krusty-gradle-plugin 1.2.3'
check 'accepted: record identity' "$(git -C "$fixture/remote.git" log -1 --format='%an <%ae> / %cn <%ce>' "$ref")" \
  'Source Author <source@example.invalid> / Source Author <source@example.invalid>'
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
run_case 'rerun while publishing' 0 $'following recorded deployment\ndeployment is PUBLISHING' ''
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
run_case failed 1 'uploaded deployment' $'Gradle output\nMaven Central rejected deployment'
rerun
echo PUBLISHING > "$fixture/states-dep-2"
run_case 'rerun after failure' 0 $'recorded deployment failed; uploading again\nuploaded deployment\ndeployment is PUBLISHING' 'Gradle output'
check 'rerun after failure: recorded' "$(recorded)" 'central deployment dep-2 of krusty-gradle-plugin 1.2.3'

# A deployment that never settles is given up on after five minutes of fake clock.
reset
echo 0 > "$fixture/uploads"
echo VALIDATING > "$fixture/states-dep-1"
run_case 'still validating' 1 'uploaded deployment' $'Gradle output\nMaven Central validation did not finish after 300s'
check 'still validating: clock' "$(cat "$fixture/clock")" 1300
check 'still validating: status calls' "$(grep -c 'status?id=dep-1' "$fixture/curl.log")" 30

# Request time counts against the five minutes: 5 s requests and 10 s pauses make 20 status
# calls, the last at 1285, and the deadline is met exactly, with no request started at 1300.
reset
echo 0 > "$fixture/uploads"
echo VALIDATING > "$fixture/states-dep-1"
echo 5 > "$fixture/status-seconds"
run_case 'slow requests' 1 'uploaded deployment' $'Gradle output\nMaven Central validation did not finish after 300s'
check 'slow requests: clock' "$(cat "$fixture/clock")" 1300
check 'slow requests: status calls' "$(grep -c 'status?id=dep-1' "$fixture/curl.log")" 20

# A status request near the deadline gets only the time that remains: with 25 s requests the
# eighth starts at 1245 with 55 s left, and the ninth starts at 1280 with a 20 s limit and times
# out at 1300.
reset
echo 0 > "$fixture/uploads"
echo VALIDATING > "$fixture/states-dep-1"
echo 25 > "$fixture/status-seconds"
run_case 'request outlasting the deadline' 28 'uploaded deployment' 'Gradle output'
check 'request outlasting the deadline: clock' "$(cat "$fixture/clock")" 1300
check 'request outlasting the deadline: limits' "$(grep 'status?id=dep-1' "$fixture/curl.log" | sed 's/.*--max-time \([0-9]*\).*/\1/' | paste -sd' ')" '60 60 60 60 60 60 60 55 20'

# A recorded deployment that cannot be looked up is not absence: with a pending deployment on
# record and the remote unreachable, nothing is built, uploaded, or recorded.
reset
echo 0 > "$fixture/uploads"
echo PUBLISHING > "$fixture/states-dep-1"
run_case 'record before outage' 0 $'uploaded deployment\ndeployment is PUBLISHING' 'Gradle output'
rerun
git -C "$tree" remote set-url origin "$fixture/unreachable.git"
# Git's own report of the unreachable remote comes first, in this Git's wording.
unreachable=$(git -C "$tree" ls-remote origin 2>&1 || true)
run_case 'unreadable record' 1 '' 'cannot read deployment journal (git ls-remote exited 128)'
check 'unreadable record: builds' "$(count "$fixture/builds")" 0
check 'unreadable record: uploads' "$(cat "$fixture/uploads")" 1
check 'unreadable record: recorded' "$(recorded)" 'central deployment dep-1 of krusty-gradle-plugin 1.2.3'

# A record that names no deployment does not authorize an upload either.
reset
echo 0 > "$fixture/uploads"
malformed=$(git -C "$tree" commit-tree "$(git -C "$tree" hash-object -t tree /dev/null)" -m 'central deployment of something else')
git -C "$tree" push -q origin "$malformed:$ref"
run_case 'malformed record' 1 '' 'deployment journal names no deployment'
check 'malformed record: builds' "$(count "$fixture/builds")" 0
check 'malformed record: uploads' "$(cat "$fixture/uploads")" 0
check 'malformed record: recorded' "$(recorded)" 'central deployment of something else' 

# A request that hits its deadline fails the step.
reset
touch "$fixture/curl-fails"
run_case 'request deadline' 28 '' ''
check 'request deadline: builds' "$(count "$fixture/builds")" 0

reset
echo 0 > "$fixture/uploads"
touch "$fixture/unsigned"
run_case unsigned 1 '' $'Gradle output\nrelease repository contains unsigned artifacts'
check 'unsigned: uploads' "$(cat "$fixture/uploads")" 0
check 'unsigned: recorded' "$(recorded)" none

# A Gradle failure identifies the exact leaf stage before any upload or journal mutation.
reset
echo 0 > "$fixture/uploads"
touch "$fixture/build-fails"
run_case 'build failure' 7 '' 'Gradle failed'
check 'build failure: final stage' "$(tail -n 1 "$fixture/progress")" "$(progress_line gradle-build 0 failed 7)"
check 'build failure: uploads' "$(cat "$fixture/uploads")" 0
check 'build failure: recorded' "$(recorded)" none

# HTTP metadata is enough to diagnose rejection without logging its private response or URL.
reset
echo 0 > "$fixture/uploads"
echo 503 > "$fixture/http-code"
run_case 'HTTP failure' 22 '' ''
check 'HTTP failure: exact progress' "$(cat "$fixture/progress")" "$(
  progress_line availability 0 start 0
  progress_line availability 0.000000 http 22 ' http_status=503'
  progress_line availability 0 failed 22
)"
check 'HTTP failure: builds' "$(count "$fixture/builds")" 0
check 'HTTP failure: uploads' "$(cat "$fixture/uploads")" 0

if [[ $failures -ne 0 ]]; then
  echo "$failures Maven Central publishing check(s) failed" >&2
  exit 1
fi
echo "Maven Central publishing: 19 cases passed"
