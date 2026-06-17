# krusty architecture — multiplatform backends

<p align="center">
  <img src="assets/krusty-mascot.webp" alt="krusty mascot" width="200">
</p>

krusty is designed as a Kotlin compiler with **pluggable backends** (JVM today; WASM and JS as
targets). The front end is backend-agnostic; everything target-specific lives behind a backend
boundary.

## Layering

```
            ┌─────────────────────────── front end (backend-agnostic) ──────────────────────────┐
  source →  lexer → parser → ast  →  resolve (type check)  →  checked program (File + SymbolTable + TypeInfo)
            └───────────────────────────────────────────────────────────────────────────────────┘
                                                                          │
                                                                          ▼
                                       ┌──────────────── backends ────────────────┐
                                       │  jvm  (.class, @Metadata, .kotlin_module) │   ← implemented
                                       │  wasm (.wasm + bindings)                  │   ← future
                                       │  js   (.js modules)                       │   ← future
                                       └───────────────────────────────────────────┘
```

- **Front end** (`token`, `lexer`, `ast`, `parser`, `types`, `resolve`): no backend dependency.
  Names, scopes, and types are expressed in **Kotlin terms** (`kotlin.String`, `kotlin.Int`, a class
  by its Kotlin FqName). It must not know JVM descriptors, WASM value types, or JS representations.
- **Backends** (`jvm`, later `wasm`/`js`): consume the checked program and lower it to the target.
  Each owns its representation decisions — e.g. on the JVM a `kotlin.Int` is an `int` or a boxed
  `java.lang.Integer` depending on context; that choice is the JVM backend's, made at its emit sites.

## Invariants

- **No non-backend module depends on a backend.** `resolve.rs`/`types.rs` must not reference
  `jvm::`. (Helpers that traffic in JVM `ClassInfo`/descriptors belong in the backend.)
- **No hardcoded type/alias tables.** Stdlib types resolve from the classpath; the Kotlin↔platform
  mapping is the ported `JavaToKotlinClassMap` (`jvm/jvm_class_map.rs`) — a *JVM-backend* table. WASM
  and JS backends carry their own mapping.
- Representation (primitive vs boxed, value-class unboxing, …) is a **backend** concern, never the
  checker's.

## Current coupling to remove (the migration)

The front end is not yet fully decoupled. The concrete blockers, in priority order:

1. **`types::Ty` conflates the Kotlin type with its JVM representation.** `Ty::Obj(&str)` stores a
   **JVM internal name** (`java/lang/String`) and `Ty::descriptor()` returns a **JVM descriptor**.
   *Target:* `Ty` references a class by **Kotlin FqName / interned class-id** (`kotlin/String`); each
   backend maps it (JVM via `JavaToKotlinClassMap`, already ported). `descriptor()` moves into `jvm`.
2. **`resolve.rs` resolves to JVM internal names** (`class_names: simple → java/lang/…`). *Target:*
   resolve to Kotlin FqNames; the JVM backend maps to internal names at lowering.
3. **`resolve::{resolve_java_static,resolve_java_instance,resolve_java_ctor}`** operate on JVM
   `ClassInfo`. *Target:* move into `jvm` as the JVM symbol provider; the checker calls an abstract
   symbol-resolution interface the backend implements.
4. **The driver** (`main.rs`) calls `jvm::emit` directly. *Target:* a `Backend` trait
   (`compile(checked program) → artifacts`); `-target jvm|wasm|js` selects the impl.

Migration is incremental and gated by the conformance harness (never regress `0 FAIL`): introduce
the `Backend` trait first (no behavior change), then move `descriptor()`/JVM-name resolution behind
it, then flip `Ty` to Kotlin FqNames with the JVM mapping at the boundary.

## The common IR (`src/ir.rs`)

The shared layer is a **high-level typed IR modeled on Kotlin IR** (`IrClass`/`IrFunction`/`IrCall`/
`IrWhen`/`IrTypeOperatorCall`/…), *not* LLVM IR or MLIR. Rationale: JVM/JS/WASM are **managed VMs**
that need Kotlin's types, nullability, and object model preserved; LLVM IR is low-level (native code,
no GC/objects) and has no JVM/JS path, and MLIR offers infrastructure but no managed-target backend
to reuse. LLVM is the right tool only for a future **native** backend (as in Kotlin/Native).

- `IrType` names classes by **Kotlin FqName** (`kotlin/Int`), never a JVM descriptor — backends map.
- Representation coercions (box/unbox, erasure) are **explicit IR nodes** (`IrTypeOp::ImplicitCoercion`)
  inserted by backend lowering, not hidden in codegen — so they are visible and testable.
- Index-based arenas (`u32` ids into `Vec`s), per krusty's no-`Box`/`Rc` invariant.

Pipeline target: `checked AST → ir (lower) → shared IR passes (desugar when/for/++, boxing form) →
per-backend lowering + codegen`. Current state: the IR **node set + builder + smoke test** exist; the
`ast → ir` lowering and the JVM backend consuming IR (replacing direct AST-to-bytecode emit) are the
next phases.
