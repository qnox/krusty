<p align="center"><img src="site/public/krusty.png" alt="krusty, a crab mascot" width="240"></p>

# krusty: a Kotlin compiler written in Rust

<p align="center">
  <a href="https://github.com/qnox/krusty/actions/workflows/ci.yml"><img src="https://github.com/qnox/krusty/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2Fkrusty-kotlin.json" alt="Latest supported Kotlin version">
  <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2Fkrusty-conformance.json" alt="Kotlin conformance">
</p>

<!-- Conformance badge = share of the Kotlin `codegen/box` suite whose `box()` returns "OK" on
     krusty-emitted bytecode. The master build recomputes it and writes the badge JSON to a Gist
     (no repo commit) — see the `release` job in .github/workflows/ci.yml. The gist id is wired via
     the CONFORMANCE_GIST_ID repo variable; updates need the GIST_TOKEN secret (PAT, `gist` scope). -->

krusty is built to be a drop-in replacement for `kotlinc` on the JVM. For the Kotlin subset it
supports, it accepts `kotlinc`'s command-line flags and aims to emit `.class` files byte-for-byte
identical to `kotlinc`'s, from a single native binary. The conformance badge shows how much of Kotlin's own test suite it passes today.

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

- Gradle and Maven integration (use the command line or Bazel)
- Compose, kapt and compiler plugins other than kotlinx.serialization (all-open, no-arg, Spring,
  Parcelize); krusty stops with an error rather than skipping them
- Kotlin scripts (`.kts`)

## Roadmap

- 100% JVM conformance with kotlinc
- Gradle integration
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
