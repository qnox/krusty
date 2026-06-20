# krusty plugin API — design PoC

Kotlin compiler extensions live in **two worlds** with very different coupling to compiler
internals. krusty must support each through a different door. This document describes the PoC in
`src/plugins/`.

## The two worlds

| World | Real Kotlin mechanism | Contract | Can mutate existing decls? | krusty door |
|---|---|---|---|---|
| **Deep compiler plugins** | FIR + IR backend extensions (Compose, kotlinx.serialization, Parcelize, all-open) | subclass concrete kotlinc IR, mutate a shared graph | yes | **native IR plugin** (`IrPlugin` trait) |
| **Codegen processors** | KSP, APT/kapt (Micronaut, Dagger, Room, Moshi) | implement stable interfaces, *read* symbols + *emit new files* | no (codegen only) | **codegen host** (`ksp` module) |

The dividing line is the API contract, not effort:

- FIR/IR extension points hand the plugin a **live, concrete `IrModuleFragment`/`FirSession`** it
  rewrites in place. You cannot shim that across a process boundary — to satisfy it you must *be*
  kotlinc. So krusty cannot host these JARs; it **reimplements** the few that matter as native
  passes over its own `IrFile`.
- KSP/APT are **interface-based and codegen-only**: input is a read-only symbol view, output is new
  source files; existing declarations are never touched. That contract *is* shim-able — a JVM
  sidecar with a shim JAR implementing `Resolver`/`KSClassDeclaration` (KSP) or `javax.lang.model`
  (APT), backed by IPC to krusty's resolver, runs the real processor JAR **unmodified**. One host
  unlocks the whole annotation-processing ecosystem.

## AST or IR? (the core placement decision)

**Neither raw AST alone nor late IR alone — split by phase, because raw AST has no resolved types.**
Kotlin never exposes the parse tree (PSI) to generation/transform plugins; every mechanism (FIR,
backend IR, KSP, APT) works on a *resolved* representation. A plugin must reason about a field's
type, a class's supertypes, whether a function is `@Composable` — none of that exists pre-resolution.

krusty's pipeline `parse → collect signatures (global) → typecheck → ir_lower → IR passes → emit`
gives two hooks that map to FIR's phase split:

| Plugin job | Hook | Level | Why this level |
|---|---|---|---|
| declaration + supertype generation (symbols user code references) | signature phase (pre-typecheck, global) | signature/AST model | generated symbols must exist *before* typecheck so references resolve |
| body generation / expression rewrite | IR pass (post-`ir_lower`) | `IrFile` | needs resolved types + descriptors; feeds the backend directly |

The forcing constraint: a class introduced only at IR level (post-typecheck) cannot have been
type-checked against by user code in the same module. `value_classes` is IR-only **only because** it
rewrites existing declarations and introduces no referenced symbols. Real declaration generation
(serialization's `serializer()`, Compose synthetics) must inject at the **signature phase**.

The PoC `IrPlugin` trait names all three phases; in this self-contained PoC they all run over
`IrFile` for testability, but the doc comments mark `generate_declarations`/`generate_supertypes` as
**production-hosted at the signature phase**, and `transform_bodies` as the genuine IR pass. KSP/APT
straddle: read a resolved view, emit source that re-enters at the parser.

## World 1 — native IR plugins (`IrPlugin`)

A native plugin is a pass over `IrFile`, exactly like `jvm::value_classes::lower_value_classes`. It
runs after `ir_lower` and before backend emit. The trait mirrors Kotlin's **three** real extension
points so a port maps method-for-method:

| `IrPlugin` method | Kotlin extension point | Job |
|---|---|---|
| `generate_supertypes` | `FirSupertypeGenerationExtension` | add interfaces/superclasses to existing classes |
| `generate_declarations` | `FirDeclarationGenerationExtension` | synthesize new classes / members |
| `transform_bodies` | `IrGenerationExtension` (backend IR) | fill in / rewrite method bodies |

`PluginHost` runs the phases **globally** (all plugins' supertypes, then all declarations, then all
bodies) — matching kotlinc's phase ordering, so a plugin can rely on another's supertypes being in
place before its declarations run.

`PluginContext` carries the annotation index (`ClassId → applied annotation FqNames`). In this PoC
it is a **side table** because `IrClass` does not yet store applied annotations (only known-flag
bools like `is_data`). The production integration is one field:

```rust
// src/ir.rs — IrClass
pub annotations: Vec<String>,   // applied annotation FqNames, populated by ir_lower
```

...populated from the AST in `ir_lower`, after which `PluginContext` reads it directly. Kept out of
this PoC to avoid editing every `IrClass` struct-literal site (the gate stays `0 FAIL`).

### Reference plugin — `serialization`

`@Serializable class Foo(val a: Int, val b: String)` → the PoC synthesizes the structure kotlinc's
serialization plugin emits:

- a nested `Foo$serializer` **object** implementing `kotlinx/serialization/KSerializer` with
  `getDescriptor`, `serialize`, `deserialize`, `childSerializers`;
- the `Foo.Companion.serializer()` accessor.

Bodies call the **real published `kotlinx-serialization-core`** runtime (`Encoder`/`Decoder`/
`SerialDescriptor`) — only codegen is native. This is the template for porting any FIR/IR plugin.

## World 2 — codegen host (`ksp`)

The host models the **codegen-only fixpoint** that KSP and APT both follow:

```
resolve → run processors over symbol view → collect generated files
        → re-resolve (generated files add symbols) → repeat until a round produces nothing new
```

PoC pieces (Rust stand-ins for the JVM shim boundary):

- `Resolver` — the read-only symbol view a processor sees (`get_symbols_with_annotation`,
  `get_all_classes`). In production this is the **shim JAR's `Resolver` impl**, each method an IPC
  call into krusty's resolver.
- `SymbolProcessor` — stands for the JVM processor JAR across the shim (`SymbolProcessorProvider`
  → `process(resolver)`).
- `CodeGenerator` — captures `createNewFile` output. **Codegen only**: it appends files, never
  touches input IR — the host asserts the input is unchanged.
- `KspHost` — drives rounds to a fixpoint with a max-round backstop.

The sample chain proves multi-round feedback: `@GenerateBuilder` → a `*Builder` annotated
`@GenerateValidator` → a `*Validator`; round 3 finds nothing new → terminate. This is the shape of
Dagger/Micronaut codegen where one generated type triggers another processor.

## The AST/signature layer is dual-use: KSP *and* a future LSP

A KSP host and an LSP server want the **same substrate**: a queryable resolved-symbol view over the
AST, carrying source spans. That is exactly KSP's `Resolver` shape. IR is the wrong layer for both —
it drops spans, assumes well-formed input, and is rebuilt per compile:

| Need | AST + resolve | IR (`IrFile`) |
|---|---|---|
| source spans (go-to-def, hover) | yes (`FunDecl.span`) | dropped |
| error-tolerant / partial code | yes | no (assumes well-formed) |
| resolved types / symbols | yes (`SymbolTable` + `TypeInfo`) | yes |
| incremental per-file | yes (krusty already streams per-file) | rebuilt per compile |
| body / codegen rewrite | no | yes |

So one front-half layer — call it `SemanticModel` (AST + resolved `SymbolTable`/`TypeInfo` + spans) —
serves **three** consumers:

- **KSP/APT host** — `Resolver` is a read adapter over it.
- **LSP** — completion / hover / go-to-def are read queries over it.
- **signature-phase decl-gen plugins** — contribute synthetic decls into it, then re-resolve.

Only **body transforms** (Compose, serialize/deserialize) need IR, and an LSP never runs the
backend — so the IR hook stays backend-only and LSP-irrelevant. This is the second, independent
reason declaration generation belongs at the AST/signature level, not IR. The PoC's KSP `Resolver`
is deliberately a read-only, span-carrying semantic view so this reuse is concrete rather than
aspirational.

## Why this split is the whole strategy

- **APT host first** — lowest coupling, reuses real `javac`; proves the front-stage fixpoint.
- **KSP host next** — biggest leverage; one host unlocks Micronaut, Dagger, Room, Moshi. Blocker:
  `Resolver` exposes generics everywhere, so krusty's `Ty`/`IrType` generic model must be complete.
- **Native passes** only for deep plugins with no codegen-only API (serialization, then maybe
  Compose — the largest single effort, version-locked to the Compose runtime ABI).

Every plugin is validated the krusty way: differential harness vs real `kotlinc` + the real plugin,
diffing ABI signatures / bytecode.
