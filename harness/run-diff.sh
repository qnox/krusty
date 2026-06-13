#!/usr/bin/env bash
# Differential test harness: krust vs the real kotlinc (ABI signatures + execution).
# Configure the reference kotlinc, then run the gated integration test.
set -euo pipefail
export KRUST_KOTLINC="${KRUST_KOTLINC:-/tmp/kdist/kotlinc/bin/kotlinc}"
export KRUST_REF_JAVA_HOME="${KRUST_REF_JAVA_HOME:-$HOME/jdks/jdk-21.0.11+10}"
export KRUST_KOTLIN_STDLIB="${KRUST_KOTLIN_STDLIB:-/tmp/kdist/kotlinc/lib/kotlin-stdlib.jar}"
export PATH="$KRUST_REF_JAVA_HOME/bin:$PATH"   # javap/javac/java from a kotlinc-compatible JDK
exec cargo test --test diff_kotlinc -- --nocapture
