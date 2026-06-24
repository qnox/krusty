#!/usr/bin/env bash
# Drive every single-file serialization boxIr corpus case through krusty end-to-end.
# Prints one line per file: PASS / FAIL:<phase> <short-reason>. Tally at the end.
set -u
ROOT="$(cd "$(dirname "$0")" && pwd)"
KR="$ROOT/target/release/krusty"
CORPUS="$ROOT/target/cache/ser-corpus/2.4.0/plugins/kotlinx-serialization/testData/boxIr"
STDLIB="$ROOT/target/cache/kotlinc/2.4.0/kotlinc/lib/kotlin-stdlib.jar"
KTEST="$ROOT/target/cache/kotlinc/2.4.0/kotlinc/lib/kotlin-test.jar"
GLIB="/opt/mise/installs/gradle/9.4.0/gradle-9.4.0/lib"
CORE="$GLIB/kotlinx-serialization-core-jvm-1.9.0.jar"
JSON="$GLIB/kotlinx-serialization-json-jvm-1.9.0.jar"
JH="${JAVA_HOME:-/opt/mise/installs/java/zulu-25.34.17.0}"
CP="$STDLIB:$KTEST:$CORE:$JSON"
pass=0; fail=0; declare -A reasons
TMP=$(mktemp -d)
for f in "$CORPUS"/*.kt; do
  base=$(basename "$f" .kt)
  # JVM facade class capitalizes the first letter of the file name; prefix the package if any
  Facade="$(echo "${base:0:1}"|tr '[:lower:]' '[:upper:]')${base:1}Kt"
  pkg=$(grep -m1 -oE '^package +[A-Za-z0-9_.]+' "$f" | awk '{print $2}')
  [ -n "$pkg" ] && Facade="$pkg.$Facade"
  # skip multi-file corpus entries (separate harness)
  if grep -qE '^// (FILE|MODULE):' "$f"; then reasons[$base]="SKIP multi-file"; continue; fi
  out="$TMP/$base"; rm -rf "$out"; mkdir -p "$out"
  cerr=$("$KR" -d "$out" -cp "$CP" "$f" 2>&1)
  if [ $? -ne 0 ]; then fail=$((fail+1)); reasons[$base]="FAIL:krusty $(echo "$cerr"|grep -m1 -iE 'error|panic|unsupported|unresolved'|sed 's#[^ ]*/##'|cut -c1-100)"; continue; fi
  printf 'public class Run{public static void main(String[] a)throws Exception{System.out.println(Class.forName("%s").getMethod("box").invoke(null));}}' "$Facade" > "$out/Run.java"
  jerr=$("$JH/bin/javac" -cp "$CP" -d "$out" "$out/Run.java" 2>&1)
  if [ $? -ne 0 ]; then fail=$((fail+1)); reasons[$base]="FAIL:javac $(echo "$jerr"|head -1|cut -c1-90)"; continue; fi
  rerr=$("$JH/bin/java" -cp "$out:$CP" Run 2>&1)
  if [ "$(echo "$rerr"|tail -1)" = "OK" ]; then pass=$((pass+1)); reasons[$base]="PASS"; else fail=$((fail+1)); reasons[$base]="FAIL:run $(echo "$rerr"|head -1|cut -c1-90)"; fi
done
for k in $(echo "${!reasons[@]}"|tr ' ' '\n'|sort); do echo "$k: ${reasons[$k]}"; done
echo "==== PASS=$pass FAIL=$fail ===="
rm -rf "$TMP"
