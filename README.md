# krusty 🤡

<p align="center">
  <img src="docs/assets/krusty-mascot.webp" alt="krusty mascot" width="320">
</p>

<p align="center">
  <a href="https://github.com/qnox/krusty/actions/workflows/ci.yml"><img src="https://github.com/qnox/krusty/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2Fkrusty-kotlin.json" alt="Supported Kotlin">
  <img src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2Fkrusty-conformance.json" alt="Kotlin conformance">
</p>

<!-- Conformance badge = share of the Kotlin `codegen/box` suite whose `box()` returns "OK" on
     krusty-emitted bytecode. The master build recomputes it and writes the badge JSON to a Gist
     (no repo commit) — see .github/workflows/conformance.yml. The gist id is wired via the
     CONFORMANCE_GIST_ID repo variable; updates need the GIST_TOKEN secret (PAT, `gist` scope). -->

> *"Hey hey! It compiles Kotlin, kids!"*

A **memory-lean Kotlin → JVM bytecode compiler** written in Rust, built as a proof of concept for a
*linear, per-file streaming* pipeline — the opposite of holding the whole program graph in memory.
The clown nose is the only thing that's a joke; the bytecode is real, and the real `kotlinc` accepts
it as a genuine Kotlin library.

Follow-up to the `kotlin-memory-bench` finding that kotlinc's whole-module pipeline is what caps
memory optimization; krusty is the per-file design built from scratch.

## What works today

krusty compiles a growing subset of Kotlin and emits `.class` files (plus `@kotlin.Metadata` and
`META-INF/*.kotlin_module`) whose **public ABI matches `kotlinc` exactly**, verified by a
differential test harness against the real compiler:

- **Top-level functions** — arithmetic (Int/Long/Double + widening), comparisons, short-circuit
  `&&`/`||`, `if`/`while`, blocks with `val`/`var` locals, string concat, calls.
- **Classes** — primary-constructor properties (`val`/`var` → backing fields + `getX`/`setX`),
  member functions (instance methods with property access), **named constructor arguments**
  (`C(b = 9)`, skipping leading literal defaults), **custom property accessors** over a backing
  field (`val x = "O"; get() = field + "K"`), and **interface delegation** to a `val`-param or an
  expression (`class D : I by Impl()`).
- **Type operators** — `is`/`as`/`as?`, including the unchecked cast to a type parameter (`x as T`,
  erased to its bound only at JVM emit), the nullable reference cast (`x as Foo?`, a `null`-passing
  `checkcast`), and the primitive→reference box cast (`42 as Any`, `b as Byte?`).
- **`@kotlin.Metadata`** — file facades (kind=2) and classes (kind=1), so a **Kotlin** consumer
  compiled by the real `kotlinc` resolves krusty's API (functions *and* classes via property
  syntax) and runs against it.
- **Java interop** — reads `.class` signatures from directories **and `.jar`s** to resolve and call
  real Java static methods; `java.lang.String` instance methods.
- **Drop-in for `kotlinc`** — accepts kotlinc-style flags (`-d`, `-classpath`/`-cp`,
  `-include-runtime`, `-module-name`, `-jvm-target`, `-version`, `-help`, …), compiles source files
  **or directories**, and writes either a directory of `.class`es or a **`.jar`** (manifest +
  classes + `<module>.kotlin_module`). Unsupported flags are ignored with a note so existing build
  invocations keep working. A jar produced by krusty is consumable by the real `kotlinc`.

## Why

Production Kotlin compilation keeps large amounts of state resident (whole-module IR, caches that
pay off only for incremental dev builds). CI builds have a different profile. krusty explores how
lean a from-scratch pipeline can be when it processes **one file at a time** with a data-oriented,
index-based AST — and whether such output can still be a drop-in Kotlin library.

## Design

- **Data-oriented AST** — every node is a `u32` index into parallel `Vec`s; a file's whole tree is
  one bulk-freeable allocation block (no pointer graph).
- **Linear pipeline** — lex → parse → collect global signatures → *per file*: typecheck → emit →
  write `.class` → drop. Only one file's codegen state is live at a time.
- **Hand-written class-file writer** — constant pool, `Code` attribute with automatic
  `max_stack`/`max_locals`, branch fixups; no external bytecode dependency.
- **Correctness by differential testing** — the source of truth is the real `kotlinc`: ABI
  signatures (`javap`) must match, and Kotlin/Java consumers must compile and run identically.
- **Conformance** — krusty is run against JetBrains/Kotlin's own `codegen/box` suite (7,352 cases):
  it skips what it can't yet compile, runs `box()` on the JVM for what it can, and is asserted to
  **never miscompile a case it accepts** (latest sweep: 476 cases compiled, all `box() == OK`, 0
  failures). Coverage grows automatically as the language widens.
- **Inline functions** — `inline fun`s are inlined from their **real compiled bytecode**, not a
  hardcoded per-function desugar: a library scope function such as `x.let { … }` / `x.also { … }` is
  resolved through the classpath and its actual stdlib body is spliced at the call site (lambda body
  included), exactly as `kotlinc`'s inliner does — no `invokestatic` to the inline callee survives.

## Layout

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

# kotlinc-style usage (krusty is a drop-in for the supported subset):
krusty src/ -d out/                          # compile a source tree to a class dir
krusty src/ -d mylib.jar -module-name mylib  # ... or to a library .jar
krusty -cp deps.jar:classes/ App.kt -d out/  # with a classpath
krusty -version | -help

# LSP server over JSON-RPC on stdin/stdout
# (full-document sync, diagnostics, hover, full/range semantic highlighting):
cargo build -p krusty-lsp
target/debug/krusty-lsp --stdio -cp deps.jar:classes/
```

Releases publish separate compiler and language-server archives for each platform.
The LSP analyzes all open Kotlin documents as one source set and uses a restartable compiler worker
to keep process-lifetime compiler interning bounded during long editor sessions.

The test harness self-provisions the reference Kotlin compiler and box corpus through `just` when
available, uses the fast `gate` profile, builds once, and runs test binaries in parallel. Pass
arguments only for a focused Cargo test/filter. `KRUSTY_TEST_JOBS=<n>` overrides full-suite binary
parallelism for profiling. Do not use `--release` for tests because the longer build cycle outweighs
the faster run. See `docs/TEST_HARNESS.md` for the agent-facing harness reference.

For performance work, use the harness output instead of inventing one-off runners. Full `./run-tests.sh`
prints the slowest test binaries. Compiler-only conformance profiling is:

```sh
KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1 ./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

That writes `target/flamegraph.svg` and prints phase timing. New JVM-running tests should use the
shared helpers in `tests/common` (`compile_and_run_box`, `run_box`, `javac_run`) so they reuse the
persistent JVM runners rather than spawning `javac`/`java` per case.

Explicit environment overrides are still supported:

```sh
KRUSTY_KOTLINC=/path/to/kotlinc/bin/kotlinc \
KRUSTY_REF_JAVA_HOME=/path/to/jdk-21 \
KRUSTY_KOTLIN_STDLIB=/path/to/kotlin-stdlib.jar \
./run-tests.sh
```

## Status

A working compiler for a real (and growing) subset, with `kotlinc`-equivalent public ABI for the
supported language, Java interop, and Kotlin-consumer round-trips passing. The roadmap
(`docs/IMPLEMENTATION_PLAN.md`) widens the language surface — data classes, secondary constructors,
class-typed members, generics, nullability — each gated by the same differential harness.
