<p align="center"><img src="site/public/krusty.png" alt="krusty, a crab mascot" width="240"></p>

# krusty: a Kotlin compiler written in Rust

<p align="center">
  <a href="https://github.com/qnox/krusty/actions/workflows/ci.yml"><img src="https://github.com/qnox/krusty/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2Fkrusty-kotlin.json" alt="Latest supported Kotlin version">
  <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2Fkrusty-conformance.json" alt="Kotlin conformance: share of applicable codegen/box cases whose box() returns OK">
  <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2Fkrusty-jvm-byte-equality.json" alt="JVM byte equality: matching leading .class bytes against kotlinc across the same codegen/box cases">
  <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2Fkrusty-native-conformance.json" alt="Native conformance: share of applicable Native codegen/box cases whose box() returns OK">
</p>

<!-- The JVM badges come from one run of the applicable Kotlin `codegen/box` cases per version.
     Applicable means kotlinc's own JVM box runner expects the case to pass:
     TARGET_BACKEND / DONT_TARGET_EXACT_BACKEND / IGNORE_BACKEND* exclusions are left out.
     Conformance badge (krusty-conformance.json) = share of those cases whose `box()` returns "OK"
     on krusty-emitted bytecode, shown as "<pct>% (<passed>/<applicable>)".
     JVM byte-equality badge (krusty-jvm-byte-equality.json) = byte equality with the same-version
     kotlinc over the same cases, shown as "<pct>% (<matched>/<total> bytes)". In every case, both
     compilers' `.class` files are paired by module and class name. A pair counts its equal leading
     bytes up to the first difference over the longer class length; a missing or extra class counts
     0 out of its full length. This exposes small forward progress while 100% still requires every
     byte of every paired class to match. A case whose box() does not return "OK" counts 0 out of
     kotlinc's class bytes. The Native badge is a separate run: passed Native cases over cases
     applicable to Native. Other test suites (serialization, KSP, and the rest of the conformance
     tests) are correctness gates only and count toward none of the badges.
     Every applicable JVM case must pass; its exact not-applicable inventory and Native's
     fail/not-applicable inventories are committed under tests/box_expected_*/<platform>/<version>.txt.
     See docs/TEST_HARNESS.md "Current Conformance".
     The master build recomputes all three and writes the badge JSON to a Gist
     (no repo commit) — see the `release` job in .github/workflows/ci.yml. The gist id is wired via
     the CONFORMANCE_GIST_ID repo variable; updates need the GIST_TOKEN secret (PAT, `gist` scope). -->

krusty is built to be a drop-in replacement for `kotlinc` on the JVM. For the Kotlin subset it
supports, it accepts `kotlinc`'s command-line flags and aims to emit `.class` files byte-for-byte
identical to `kotlinc`'s, from a single native binary. Three badges measure it against the applicable
cases of Kotlin's own `codegen/box` test suite. The conformance badge shows how much of that suite
it passes today: the share of cases whose `box()` test returns `OK`. The JVM byte-equality badge
shows how close its output is to `kotlinc`'s on the same cases: the matching leading bytes of each
paired `.class` file over the longer file length. The Native conformance badge independently shows
the share of Native-applicable cases that compile, link, run, and return `OK`.

[Website](https://krustythecompiler.dev) · [Download](https://github.com/qnox/krusty/releases/latest) · [Sponsor](https://github.com/sponsors/qnox)

## Install

Download the archive for your platform from the
[latest release](https://github.com/qnox/krusty/releases/latest) (Linux, macOS and Windows, on
x86_64 and arm64), extract `krusty`, and put it on your `PATH`.

krusty itself needs no JVM, but it reads the JDK's class library, so a JDK must be installed
(`JAVA_HOME` or `-jdk-home <dir>`).

## Usage

```sh
krusty src/ -d out/                          # compile a source tree to a class directory
krusty src/ -d mylib.jar -module-name mylib  # or to a jar
krusty -cp deps.jar:classes/ App.kt -d out/  # with a classpath
krusty -help                                 # all options
```

The reference `kotlinc` version krusty targets defaults to the newest supported release; select
another with `-Xkotlin-reference-version=<version>`.

### Gradle

Download `krusty-gradle-plugin-<version>.zip` from the
[latest GitHub release](https://github.com/qnox/krusty/releases/latest) and unpack it under the
build's `gradle/` directory. It contains a portable local Maven repository; krusty is not published
to the Gradle Plugin Portal or Maven Central.

```kotlin
// settings.gradle.kts
pluginManagement {
    repositories {
        maven { url = uri("gradle/krusty-gradle-plugin-<version>/repository") }
        gradlePluginPortal()
        mavenCentral()
    }
}
```

```kotlin
// build.gradle.kts
plugins {
    kotlin("jvm") version "2.4.20" // also supported: 2.4.0 and 2.4.10
    id("krusty") version "<version>"
}
```

Kotlin/JVM must be applied before krusty. `kotlin("plugin.serialization")` works in any order:
krusty receives the Kotlin Gradle plugin's compiler-plugin classpath and options exactly as kotlinc
would. The compiler identifies plugin jars and directories from their registrar service entries;
any unsupported Kotlin compiler plugin (sam-with-receiver, Compose, ...) fails the build regardless
of the artifact filename. KSP2
(`com.google.devtools.ksp`) works: it runs processors in its own `kspKotlin` task and
adds no compiler plugin to the compilation, and krusty compiles the Kotlin and Java sources the
processors generate (KSP's Gradle plugin needs Gradle 8 or newer). The
validated Gradle versions are 7.6.3, 8.14.3, 9.5.0, and 9.7.0. Projects applying Gradle's
`kotlin-dsl` (such as `buildSrc`) and Kotlin compile tasks outside any source set stay with kotlinc;
the plugin logs each task it leaves. Point the plugin at the compiler binary with either form:

```sh
./gradlew -Pkrusty.binary=/absolute/path/to/krusty krustyCompile
KRUSTY_BIN=/absolute/path/to/krusty ./gradlew krustyCompile
```

## Features

- **kotlinc-compatible command line** for the supported subset, output to a class directory or a jar.
- **Byte-level fidelity.** Every change is diffed against the real `kotlinc`, and every build runs
  JetBrains' `codegen/box` suite.
- **Low memory.** On our multi-module benchmark, about a third of the peak memory of the
  GraalVM-native `kotlinc`.
- **kotlinx.serialization and KSP.**
- **Bazel** persistent worker support.
- **Editor support.** `krusty-lsp` language server, with a [Zed extension](editors/zed/README.md).

## Status

krusty compiles plain Kotlin/JVM code today. Not supported yet:

- Maven integration (use the command line, Gradle plugin or Bazel)
- Compose, kapt and compiler plugins other than kotlinx.serialization, all-open and no-arg
  (sam-with-receiver, Parcelize); krusty stops with an error rather than skipping them
- Kotlin scripts (`.kts`)

## Roadmap

- 100% JVM conformance with kotlinc
- Kotlin/Native
- Kotlin/Wasm
- Compose

## Sponsor

If krusty saves your team build time or CI money, please consider sending part of it back through
[GitHub Sponsors](https://github.com/sponsors/qnox).

## Building from source

```sh
cargo build --release -p krusty-cli   # the compiler
cargo build --release -p krusty-lsp   # the language server
./run-tests.sh                        # full test suite against the real kotlinc
```

With [`just`](https://github.com/casey/just) installed, the test harness downloads the reference
`kotlinc` and test corpus itself. See
[`docs/TEST_HARNESS.md`](docs/TEST_HARNESS.md) for details, [`AGENTS.md`](AGENTS.md) for contributor
rules, and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for how the compiler is organized.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Unless you explicitly state otherwise,
any contribution intentionally submitted for inclusion in this work shall be licensed as above,
without any additional terms or conditions.
