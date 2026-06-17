# krusty 🤡

<p align="center">
  <img src="docs/assets/krusty-mascot.webp" alt="krusty mascot" width="320">
</p>

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
  member functions (instance methods with property access).
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
- **Conformance** — krusty is run against JetBrains/Kotlin's own `codegen/box` suite (10,009 cases):
  it skips what it can't yet compile, runs `box()` for what it can, and is asserted to **never
  miscompile a case it accepts** (latest sweep: 13 in-subset cases, all `OK`, 0 failures). Coverage
  grows automatically as the language widens.

## Layout

```
src/lexer.rs, parser.rs, ast.rs   front end (Pratt expressions, arena AST)
src/types.rs, resolve.rs          type model + signature collection + per-file typecheck
src/codegen/                      class-file writer + AST→bytecode lowering
src/metadata/                     @kotlin.Metadata protobuf + .kotlin_module emitters
src/jvm/                          .class reader + jar/dir classpath
tests/                            differential + round-trip harness vs real kotlinc
docs/SPEC.md                      language subset + Kotlin-semantics decisions
docs/IMPLEMENTATION_PLAN.md       phased plan (each phase ends green)
docs/METADATA_NOTES.md            reverse-engineered @Metadata schema
```

## Build & test

```sh
cargo build
cargo test                       # unit + e2e (kotlinc-gated tests skip without env)

# kotlinc-style usage (krusty is a drop-in for the supported subset):
krusty src/ -d out/                          # compile a source tree to a class dir
krusty src/ -d mylib.jar -module-name mylib  # ... or to a library .jar
krusty -cp deps.jar:classes/ App.kt -d out/  # with a classpath
krusty -version | -help
```

The differential tests against the real compiler are opt-in via environment variables:

```sh
KRUSTY_KOTLINC=/path/to/kotlinc/bin/kotlinc \
KRUSTY_REF_JAVA_HOME=/path/to/jdk-21 \
KRUSTY_KOTLIN_STDLIB=/path/to/kotlin-stdlib.jar \
cargo test
```

## Status

A working compiler for a real (and growing) subset, with `kotlinc`-equivalent public ABI for the
supported language, Java interop, and Kotlin-consumer round-trips passing. The roadmap
(`docs/IMPLEMENTATION_PLAN.md`) widens the language surface — data classes, secondary constructors,
class-typed members, generics, nullability — each gated by the same differential harness.
