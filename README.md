# krusty

<p align="center">
  <a href="https://github.com/qnox/krusty/actions/workflows/ci.yml"><img src="https://github.com/qnox/krusty/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2Fkrusty-kotlin.json" alt="Supported Kotlin">
  <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2Fkrusty-conformance.json" alt="Kotlin conformance">
</p>

<!-- Conformance badge = share of the Kotlin `codegen/box` suite whose `box()` returns "OK" on
     krusty-emitted bytecode. The master build recomputes it and writes the badge JSON to a Gist
     (no repo commit) — see .github/workflows/conformance.yml. The gist id is wired via the
     CONFORMANCE_GIST_ID repo variable; updates need the GIST_TOKEN secret (PAT, `gist` scope). -->

**krusty** is a memory-conscious **Kotlin → JVM bytecode compiler** written in Rust. Its frontend
builds a module-wide source and symbol view. Its backend lowers one checked file per call while
carrying the state needed to finalize module metadata. It emits `.class` files,
`@kotlin.Metadata`, and `META-INF/*.kotlin_module` for the supported language subset.

**Goal:** emit bytecode that is **byte-for-byte identical** to the reference `kotlinc` for every
construct krusty supports, not only ABI-compatible output. Differential bytecode tests, Kotlin/Java
consumer tests, and the Kotlin `codegen/box` corpus provide the oracle for that goal.

---

## Contents

- [Motivation](#motivation)
- [Why memory-lean matters](#why-memory-lean-matters)
- [What makes it different](#what-makes-it-different)
- [Compiler plugins](#compiler-plugins)
- [Design](#design)
- [Project layout](#project-layout)
- [Build & test](#build--test)
- [Language server](#language-server)
  - [Zed setup](#zed-setup)
- [Status](#status)

---

## Motivation

krusty began as an agent-driven experiment: could a Kotlin compiler be rebuilt in Rust and driven by
differential tests all the way to executable JVM bytecode? The implementation explores compact,
index-based compiler data and file-oriented backend lowering while retaining the module-wide
frontend state needed for Kotlin name and type resolution.

## Why memory-lean matters

Compilation is repeated across local builds, pull requests, merge queues, and version matrices.
Memory use limits how many jobs can share a runner and which runner size a build requires, so reducing
resident compiler state can improve capacity as well as latency.

The compiler currently retains parsed files, module symbols, and checked frontend data for the
source set, so total memory is not bounded by the largest source file. The backend API processes one
checked file per `lower_file` call and carries explicit module state into finalization. This boundary
makes memory behavior measurable and leaves room to shorten frontend lifetimes without changing the
backend contract.

## What makes it different

Compiling Kotlin to JVM bytecode is the baseline. krusty focuses on these implementation properties:

- **Byte-level fidelity.** Tests compare emitted classes and metadata with `kotlinc`, then compile
  Kotlin and Java consumers against krusty's output.
- **Classpath bytecode inlining.** Supported inline calls can splice the selected callable's
  compiled body instead of relying on a function-name-specific rewrite.
- **Explicit plugin paths.** kotlinx.serialization uses a native IR pass; KSP uses a code-generation
  host for external processors. The general Kotlin compiler-plugin ABI is not exposed.
- **File-oriented backend contract.** Module-wide resolution feeds one checked file per backend call,
  with explicit state carried into module finalization.

It offers a **kotlinc-compatible command line for the supported subset**: kotlinc-style flags in, a
`.class` directory or `.jar` out. The supported language subset lives in
[`docs/SPEC.md`](docs/SPEC.md); the badges above track the current Kotlin version and `codegen/box`
conformance.

## Compiler plugins

krusty does not implement the general Kotlin compiler-plugin extension ABI. Plugin behavior is
integrated through narrower contracts that the compiler can validate explicitly:

- **kotlinx.serialization** uses a native IR pass for `@Serializable` synthesis
  ([`src/plugins/serialization.rs`](src/plugins/serialization.rs)).
- **KSP (Kotlin Symbol Processing)** uses a version-pinned code-generation host for external KSP
  processors and feeds generated source back through compilation
  ([`src/plugins/ksp.rs`](src/plugins/ksp.rs)).

Other third-party Kotlin compiler plugins are currently unsupported. See
[`docs/PLUGIN_API.md`](docs/PLUGIN_API.md) for the implemented paths, test matrix, and remaining
limitations.

## Design

- **Data-oriented AST** — nodes are arena indices rather than a pointer graph. Each file has its own
  arenas, while the source set remains available for module-wide checking.
- **Module frontend, file backend** — parse source set → collect module signatures → check source
  set → lower each checked file → finalize module artifacts.
- **Hand-written class-file writer** — constant pool, `Code` attribute with automatic
  `max_stack`/`max_locals`, branch fixups; no external bytecode dependency.
- **Correctness by differential testing** — the source of truth is the real `kotlinc`: ABI
  signatures (`javap`) must match, and Kotlin/Java consumers must compile and run identically.
- **Conformance** — krusty runs against JetBrains/Kotlin's own `codegen/box` suite: it skips what it
  can't yet compile, runs `box()` on the JVM for what it can, and is asserted to **never miscompile a
  case it accepts**. The conformance badge reports the current pass share.

## Project layout

```
src/lexer.rs, parser.rs, ast.rs   front end (Pratt expressions, arena AST)
src/types.rs, resolve.rs          type model + signature collection + per-file typecheck
src/ir.rs, ir_lower.rs            backend-neutral IR + AST→IR lowering
src/jvm/                          IR→bytecode emit, class-file writer, .class reader, jar/dir
                                  classpath, bytecode inliner (inline.rs)
src/metadata/                     @kotlin.Metadata protobuf + .kotlin_module emitters
crates/krusty-cli/                kotlinc-compatible batch executable and command parsing
crates/krusty-lsp/                compiler-backed analysis, JSON-RPC/LSP, compact query state
tests/                            differential + round-trip harness vs real kotlinc
docs/SPEC.md                      language subset + Kotlin-semantics decisions
docs/IMPLEMENTATION_PLAN.md       phased plan (each phase ends green)
docs/METADATA_NOTES.md            reverse-engineered @Metadata schema
```

## Build & test

```sh
cargo build
./run-tests.sh                   # normal full-suite gate; no parameters needed
just test                        # equivalent harness entrypoint
```

kotlinc-style usage for the supported subset:

```sh
krusty src/ -d out/                          # compile a source tree to a class dir
krusty src/ -d mylib.jar -module-name mylib  # ... or to a library .jar
krusty -cp deps.jar:classes/ App.kt -d out/  # with a classpath
krusty -version | -help
```

The harness self-provisions the reference Kotlin compiler and box corpus through `just` when
available, uses the fast `gate` profile, builds once, and runs test binaries in parallel. Pass
arguments only for a focused Cargo test/filter. Do not use `--release` for tests: the longer build
cycle outweighs the faster run. See [`docs/TEST_HARNESS.md`](docs/TEST_HARNESS.md) for the full
harness reference, including profiling knobs and the `KRUSTY_KOTLINC` / `KRUSTY_REF_JAVA_HOME` /
`KRUSTY_KOTLIN_STDLIB` environment overrides.

## Language server

krusty ships a compiler-backed LSP server over JSON-RPC (stdin/stdout). Releases publish separate
compiler and language-server archives per platform.

```sh
cargo build -p krusty-lsp
target/debug/krusty-lsp --stdio -cp deps.jar:classes/
```

It analyzes all open Kotlin documents as one source set through a restartable compiler worker that
keeps process-lifetime interning bounded over long editor sessions. Supported requests:

- diagnostics, semantic highlighting, hover
- completion (with resolve) and signature help
- go-to-definition, -type-definition, and -implementation
- find references and rename
- hierarchical document symbols

Navigation and symbol data are served from compact, interned, integer-indexed snapshots rather than
retained compiler ASTs. A restartable worker limits the lifetime of compiler analysis state.

### Zed setup

Zed can't launch an arbitrary server from `settings.json`, so krusty ships a small dev extension in
[`editors/zed`](editors/zed) that registers `krusty-lsp` as a second server for the `Kotlin`
language (grammar and syntax still come from the official Kotlin extension). Quick start:

1. Build the server: `cargo build --release -p krusty-lsp`.
2. Install the **Kotlin** extension from Zed's gallery (for the tree-sitter grammar).
3. Install this repo's extension: command palette → `zed: install dev extension` → select
   `editors/zed`.
4. Point Zed at the binary and turn off the other Kotlin servers in `settings.json`:

   ```json
   {
     "languages": {
       "Kotlin": {
         "language_servers": ["krusty-lsp", "!kotlin-lsp", "!kotlin-language-server", "..."]
       }
     },
     "lsp": {
       "krusty-lsp": {
         "binary": {
           "path": "/absolute/path/to/krusty/target/release/krusty-lsp",
           "arguments": ["--stdio"]
         }
       }
     }
   }
   ```

   Omit `binary.path` to take `krusty-lsp` from `PATH`.

The extension forwards the worktree shell environment, so `JAVA_HOME` (from the shell, mise, or
direnv) reaches the server; without a JDK it resolves no `java.*` symbols. The server detects BSP /
Gradle / Maven project models and refreshes the classpath on build-file changes; passing `-cp` in
`arguments` pins a fixed classpath instead. The extension launches the same server and capabilities
listed above. See [`editors/zed/README.md`](editors/zed/README.md) for the full guide.

## Status

A working compiler for a real, growing subset of Kotlin, with `kotlinc`-matching output for what it
supports, Java interop, and Kotlin-consumer round-trips passing. The roadmap in
[`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md) widens the language surface, each step
gated by the same differential harness. It is a proof of concept, not yet a production compiler.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Unless you explicitly state otherwise,
any contribution intentionally submitted for inclusion in this work shall be licensed as above,
without any additional terms or conditions.
