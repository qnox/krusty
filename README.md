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

krusty is a drop-in replacement for `kotlinc` on the JVM. It takes the same command-line flags and
emits `.class` files byte-for-byte identical to `kotlinc`'s, from a single native binary.

[Website](https://krustythecompiler.dev) · [Download](https://github.com/qnox/krusty/releases/latest) · [Sponsor](https://github.com/sponsors/qnox)

## Install

Download the archive for your platform from the
[latest release](https://github.com/qnox/krusty/releases/latest) (Linux, macOS and Windows, on
x86_64 and arm64), extract `krusty`, and put it on your `PATH`.

Requires a JDK (`JAVA_HOME` or `-jdk-home <dir>`).

## Usage

```sh
krusty src/ -d out/                          # compile a source tree to a class directory
krusty src/ -d mylib.jar -module-name mylib  # or to a jar
krusty -cp deps.jar:classes/ App.kt -d out/  # with a classpath
krusty -help                                 # all options
```

Output matches the newest supported Kotlin release; pick another with
`-Xkotlin-reference-version=<version>`.

## Features

- **Drop-in for kotlinc.** Same flags, byte-for-byte identical `.class` files and jars.
- **Light on resources.** A single native binary using about a third of kotlinc's memory.
- **kotlinx.serialization and KSP** work out of the box.
- **Bazel** persistent worker support.
- **Editor support.** `krusty-lsp` language server, with a [Zed extension](editors/zed/README.md).

## Roadmap

- 100% JVM conformance with kotlinc
- Kotlin/Native
- Kotlin/Wasm
- Compose

## Sponsor

krusty needs far less memory than kotlinc, so your CI needs fewer and smaller runners. If that saves
your organization money, please consider sending part of it back through
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
