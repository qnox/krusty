#!/usr/bin/env bash
# Differential test harness: krusty vs the real kotlinc (ABI signatures + execution).
# Configure the reference kotlinc, then run the gated integration test.
set -euo pipefail
export KRUSTY_KOTLINC="${KRUSTY_KOTLINC:-/tmp/kdist/kotlinc/bin/kotlinc}"
export KRUSTY_REF_JAVA_HOME="${KRUSTY_REF_JAVA_HOME:-$HOME/jdks/jdk-21.0.11+10}"
export KRUSTY_KOTLIN_STDLIB="${KRUSTY_KOTLIN_STDLIB:-/tmp/kdist/kotlinc/lib/kotlin-stdlib.jar}"
export PATH="$KRUSTY_REF_JAVA_HOME/bin:$PATH"   # javap/javac/java from a kotlinc-compatible JDK
exec cargo test --test diff_kotlinc -- --nocapture
