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
- **Lowering split:** common `ir_lower` may desugar Kotlin semantics only. Target/runtime-dependent
  rewrites (JVM callable-reference classes, captured-var `Ref$*Ref` holders, counted-loop range
  optimizations, primitive/boxed ABI choices, value-class erasure, suspend CPS) belong in named
  backend lowering passes. If a temporary common-lowering hook is needed while the IR lacks a neutral
  node, keep the hook narrow, backend-owned, and record it as migration debt rather than adding JVM
  spelling or platform policy to core lowering.
- **Process front ends** are separate workspace packages. The root `krusty` package is a compiler
  library and exposes frontend and backend contracts. `crates/krusty-cli` owns kotlinc-compatible
  batch argument parsing, filesystem output, and process exit behavior, while `crates/krusty-lsp`
  owns its in-memory source-set analysis, JSON-RPC, document lifecycle, and compact editor query
  snapshots. These packages depend toward the compiler library; the compiler never depends on any
  process adapter. LSP compiler analysis is an internal module—not a single-consumer workspace
  package—and architecture guards keep it isolated from the long-lived protocol/session modules.
- **Shared process-independent policy** stays in the compiler library when it is genuinely a compiler
  concern. For example, JVM classpath code resolves a JDK home to `lib/modules`; each executable
  independently decides how its own arguments select that home. There is no command-layer “common”
  crate until both executable packages share a stable command abstraction rather than a few flags.

## Language-server memory model

- `serde`, `serde_json`, JSON-RPC transport, and session state belong to the separate
  `crates/krusty-lsp` workspace package. The compiler's dependency graph has no server dependency
  or server-specific feature. Within the LSP package, `compiler_analysis` is the only module allowed
  to inspect checked frontend data; protocol/session modules consume compact snapshot contracts.
- The LSP supervisor never runs the compiler in its own long-lived process. It sends source sets to
  a compiler worker that is restarted after 64 analyses. This bounds growth from the compiler's
  process-lifetime name/type interners while amortizing JVM classpath initialization across edits.
- An open document retains its source text, diagnostics only long enough to publish them, a compact
  hover index, completion catalog, definition index, and semantic-highlighting tokens. Each hover
  entry is a 12-byte `(Span, type-id)` record; rendered type names are deduplicated per document. A
  scoped completion entry is a 24-byte packed array of scope bounds, declaration position, interned
  label/detail IDs, item kind, and optional receiver type; member entries are 16 bytes. Completion and
  item resolution filter these cached records, including parser-recovered `receiver.`/`receiver?.`
  expressions,
  without retaining the AST or invoking the worker. A document retains member catalogs only for
  receiver types referenced by its own lexical symbols/source, rather than duplicating every member
  in the open source set. A shared source-set budget caps completion at 32,768 records and a
  conservative 4 MiB wire estimate; a truncated snapshot reports `isIncomplete: true`. Each
  semantic token is a 16-byte `(UTF-16 line, start, length, type, modifiers)` record, positioned once
  in the compiler worker so full/range requests neither rerun analysis nor rescan source. Worker JSON
  uses packed array entries rather than repeating object keys, and range encoding binary-searches
  the sorted snapshot before allocating its result. A definition entry is a 20-byte
  `(source lo, source hi, target file, target lo, target hi)` array with no retained strings; a shared
  256K-entry budget bounds both construction and long-lived storage.
- Open documents are analyzed as one source set, so one parse/signature pass resolves declarations
  across open files and refreshes every open file's diagnostics, completion, hover, and highlighting
  snapshots atomically. Temporary source-set catalogs carry completion declarations and source-only
  highlighting flags such as `data`, `operator`, and `Deprecated` across files while the compact
  snapshots are built. Navigation also consumes checker-selected source declaration ids for overloads
  before reducing them to file/span pairs. AST, symbol-table, full type-analysis, and those catalogs
  are dropped after each analysis; closing a document removes its source and compact query indexes.
- Input frames are capped at 16 MiB, headers at 8 KiB, and the reader-to-dispatch queue at four
  parsed messages. Open text is capped at 32 MiB across at most 256 documents; worker JSON encoding
  is capped at 64 MiB in both directions. Document-state bursts are capped by count, retained bytes,
  and elapsed time; their newest changes are applied in one analysis. A worker analysis is terminated
  and restarted after 30 seconds.
- The server advertises full-document synchronization. This avoids a second rope/piece-table
  representation and makes each accepted version replace the prior text allocation; stale versions
  do not trigger analysis.

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

1. **`types::Ty` still conflates Kotlin semantic identity with target/runtime shape.** JVM descriptor
   formatting has moved out of `Ty`, but `Ty::Obj(&str)` still stores names that are sometimes Kotlin
   builtins and sometimes JVM/internal runtime classes (`java/lang/String`, `kotlin/jvm/functions/*`).
   Some non-backend code also still reasons about boxed primitives, nullable scalar wrappers, and
   value-class representation. *Target:* `Ty` references a Kotlin class-id; each backend maps it to its
   own ABI and runtime names.
2. **`resolve.rs` and common lowering still contain JVM-shaped facts.** Examples include direct
   `java/lang/*` names, function-interface names, boxed-wrapper assumptions, `$default` awareness, and
   value-class erasure checks. *Target:* checker/lowerer select semantic calls/properties/types through
   `SymbolSource`/`CallResolver`; JVM ABI decisions happen in JVM lowering/emission.
3. **Checker and lowerer duplicate call selection.** The newer provider boundary is `SymbolSource` plus
   `FunctionSet`/`FunctionInfo` and `CallResolver`, but `TypeInfo` still carries feature-specific side
   maps and `ir_lower.rs` often re-resolves what the checker selected. *Target:* one resolved-call /
   resolved-property table carries selected callable identity, argument mapping, metadata facts, and the
   backend handle forward.
4. **The batch driver** (`crates/krusty-cli/src/main.rs`) selects the JVM backend directly. *Target:*
   backend selection remains executable policy while compilation is expressed through the `Backend` trait
   (`compile(checked program) → artifacts`); `-target jvm|wasm|js` selects the impl.

Migration is incremental and gated by the conformance harness (never regress `0 FAIL`): introduce
the `Backend` trait first (no behavior change), then carry selected calls/properties through a
backend-neutral handle, then flip `Ty` to Kotlin class ids with JVM mapping at the backend boundary.

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

Pipeline target: `checked AST → common IR → shared semantic passes → per-backend lowering + codegen`.
Current state: the JVM backend consumes the current IR, but that IR is still partly JVM-lowered: common
`Callee` forms carry owners, names, descriptors, `$default` and `INSTANCE` knowledge, and some backend
policy is still decided in `ir_lower.rs`. The migration target is a clean split between common semantic
IR and JVM-lowered IR.
