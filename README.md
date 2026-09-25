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

[Website](https://krustythecompiler.dev) · [Download](https://github.com/qnox/krusty/releases/latest)

## Install

Download the archive for your platform from the
[latest release](https://github.com/qnox/krusty/releases/latest) (Linux, macOS and Windows, on
x86_64 and arm64), extract `krusty`, and put it on your `PATH`.

krusty needs a JDK for `java.*` classes: it uses `JAVA_HOME`, or pass `-jdk-home <dir>`. It takes
`kotlin-stdlib` from your Gradle or Maven cache, or downloads it from Maven Central; pass
`-no-stdlib` to supply your own on the classpath.

## Usage

```sh
krusty src/ -d out/                          # compile a source tree to a class directory
krusty src/ -d mylib.jar -module-name mylib  # or to a jar
krusty -cp deps.jar:classes/ App.kt -d out/  # with a classpath
krusty -help                                 # all options
```

krusty matches the newest supported Kotlin release by default. To match an older one, pass
`-Xkotlin-reference-version=<version>`.

## Features

- **Compiles Kotlin to the JVM.** Output is `.class` files, `@kotlin.Metadata` and
  `META-INF/*.kotlin_module`, written to a directory or a `.jar`.
- **Takes kotlinc's command line.** The same flags work, and `krusty` also runs as a Bazel
  persistent worker.
- **Matches kotlinc byte for byte.** Differential tests compare each emitted class with the real
  `kotlinc`'s, then compile Kotlin and Java consumers against krusty's output. Every build also runs
  JetBrains' `codegen/box` tests; the conformance badge shows the share that passes.
- **Inlines from compiled libraries.** Calls to `inline` functions in library jars copy the callee's
  compiled bytecode, as `kotlinc` does.
- **Supports kotlinx.serialization and KSP.** Serialization runs as a built-in compiler pass, and KSP
  processors run through a bundled host. Other compiler plugins are not supported yet.

## Editor support

`krusty-lsp` is a Kotlin language server built on the same compiler. It provides diagnostics,
completion, hover, signature help, go-to-definition, find references, rename and document symbols,
and reads Gradle, Maven and BSP project models. Each release ships it for every platform, together
with a [Zed extension](editors/zed/README.md).

## Roadmap

- **Full JVM conformance:** every Kotlin `codegen/box` test passing with output identical to
  `kotlinc`'s.
- **Kotlin/Native:** in progress. A native backend with its own runtime, code generator and linker,
  reading Kotlin/Native's own standard library.
- **Kotlin/Wasm:** planned.
- **Compose:** planned. Support for the Jetpack Compose compiler plugin.

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
