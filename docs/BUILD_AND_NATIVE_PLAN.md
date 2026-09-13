# Build system and native target — proposal

**Status: proposal.** Nothing here is implemented. This document scopes two related bets, records
the seams that already exist for them, and sequences the work so that every phase ends green and
ships value on its own.

The two bets:

1. **A Go-like build layer** — one binary that reads a module graph, hashes inputs, skips work whose
   ABI inputs are unchanged, and schedules the rest across cores. Targets the JVM first, where krusty
   already emits correct artifacts.
2. **A Kotlin/Native-shaped target** — klib in, native code out, under the Kotlin Multiplatform
   contract (no Java interop). Justified only if it is *fast*; Kotlin/Native is LLVM-based and
   whole-program, and that is the gap.

They share a spine. Phases 1–4 below are prerequisites for both; only phases 5–6 are native-only.

References below cite **files and symbols** rather than line numbers, which rot quickly on this
repository.

---

## Contents

- [Thesis](#thesis)
- [Scope and non-goals](#scope-and-non-goals)
- [What already exists](#what-already-exists)
- [What does not exist](#what-does-not-exist)
- [Architecture](#architecture)
- [Phases](#phases)
- [Correctness strategy](#correctness-strategy)
- [Risks and open questions](#risks-and-open-questions)

---

## Thesis

**Build speed is a build-model property, not only a compiler property.** `go build` is fast because
of per-package units, compact export data, a content-addressed cache keyed on input and dependency
ABI hashes, a parallel DAG, and a compiler that starts instantly. krusty already supplies the last
of those — a Rust binary with no VM startup, no daemon warmup, no configuration phase. The rest is
missing, and it is conventional engineering.

**Kotlin/Native is slow for structural reasons, not incidental ones.** It runs LLVM with
optimization passes, and it compiles whole-program: every dependency klib is deserialized and
lowered again at each link of the final binary, with no per-module native artifact cache worth the
name. That is the anti-Go model. A target that used a fast code generator and cached per-module
native artifacts would occupy an opening nobody in the Kotlin ecosystem occupies today.

**Multiplatform removes the objection that killed native before.** Java interop is out of contract
for a native target — Kotlin/Native never had it; its interop is C and Objective-C/Swift. So a
native target does not forfeit krusty's identity. It adopts the same target contract JetBrains
already defined: klibs, `expect`/`actual`, the common stdlib.

**klib is a better library format than the classpath, not a worse one.** The compiler's deepest JVM
coupling has been that the semantic model of the stdlib is read out of `.class` files, and that
`inline fun` is realized by splicing compiled bytecode (`src/jvm/inline.rs`, which relocates
constant-pool indices between class files). A klib carries serialized Kotlin IR *including inline
function bodies*, so the native path replaces bytecode splicing with IR-level inlining — the
architecturally cleaner design. The hardest-looking blocker dissolves into a deserializer, and the
codebase has already started moving this way on its own (see `InlineBodyPlan`, below).

---

## Scope and non-goals

**In scope.** A `krusty build` driver with a module graph, ABI hashing, and a content-addressed
output cache. klib ingestion and emission. A native code generator and the minimum runtime to run
the `codegen/box` corpus on Linux x86-64 and arm64.

**Not in scope, deliberately.**

- *Java interop on the native target.* Out of contract. The JVM target keeps it; the native target
  never has it. `src/java_source.rs` and `src/jvm/java_stub.rs` stay JVM-only.
- *Byte-identical native binaries.* The JVM target's bar is byte parity with `kotlinc`. No such bar
  exists or should exist for native output; the oracle is behavioral (see
  [Correctness strategy](#correctness-strategy)). This is a genuine loosening of constraint.
- *Replacing Gradle.* The build layer reads existing Gradle/Maven/BSP/JPS models. A native manifest
  format is a later option, never an adoption prerequisite.
- *Objective-C/Swift export and cinterop.* Last, if ever. The build-speed thesis is provable on
  Linux first.
- *A second general-purpose IR below the current one for the JVM target.* The native lowering gets
  its own low IR; the JVM path is not rerouted through it.

---

## What already exists

The seams are considerably further along than the roadmap suggests — and the recent FIR streaming
work moved them further still.

### The target contract is a named trait tower

```
SymbolSource            src/symbol_source.rs     where declarations come from
  └─ SemanticPlatform   src/libraries.rs         library semantics in Kotlin terms
TargetRuntime           src/runtime.rs           platform ABI services (~25 default-None methods)
  └─ CompilerPlatform   src/runtime.rs           SemanticPlatform + TargetRuntime, blanket impl
Backend                 src/backend.rs           lower_file / lower_ir_file / finalize → Vec<Artifact>
```

A target is a `CompilerPlatform` plus a `Backend`. `src/jvm/jvm_libraries.rs` (~9,000 lines) is the
JVM implementation of the first; `src/jvm/backend.rs` is the second. Common lowering is generic over
`TargetRuntime`, so it asks the platform for descriptors, range constructors, boxing shapes, and
runtime helpers rather than spelling them. **A klib platform slots in beside the JVM one without
inventing a new abstraction** — `CompilerPlatform` has a blanket impl, so implementing the two
halves is sufficient.

### The backend boundary is now a streaming IR boundary

`Backend` carries a second entry point, `lower_ir_file(CheckedIrFile)`, and the main driver in
`src/compiler.rs` uses it. Its contract is strict, in the code's own words: *"No parsed source or
AST-keyed semantic table crosses this boundary; target realization consumes only checked IR and
compact stable module facts."*

The accompanying `src/backend/module_facts.rs` defines `BackendClassifierSource` — *"the only
semantic query a representation backend may make after common lowering"* — returning frozen
`BackendClassifierFact` records guaranteed free of `Ty::Pending`, `Ty::Error`, source type
references, and source-body payloads.

This matters more than anything else in this section. **A backend now consumes (checked IR + frozen,
finalized classifier facts) and nothing else.** That is a serializable, target-neutral input — which
is precisely what both a cache key and a native backend need. The older `lower_file(CheckedFile)`
path still exists alongside it; the migration is in flight.

### The frontend already separates headers from bodies

`src/fir/` (~29,000 lines) is documented as a *"streaming frontend ownership model"* whose module
tree *"follows frontend lifetime boundaries: stable header inventory, temporary signature solving,
checked body ownership."* It contains `header.rs`, `signature.rs` (~3,600 lines), and
`signature_extract.rs` (~2,900 lines); `src/fir_lower/` adds ~22,000 more.

**A stable header inventory separated from body checking is the ABI/Impl split, at the frontend
level, already built.** Phase 2 below is largely a matter of serializing and hashing what this layer
already computes, rather than deriving it from scratch.

### Inline bodies are already modeled semantically

`InlineBodyPlan` in `src/libraries.rs` describes stdlib inline functions as structured plans —
`InvokeLambda { lambda_parameter, argument_parameters, return_parameter }`, `CollectionTransform
{ lambda_parameter, flatten, .. }`, `SuspendBeforeLambdaFinally { .. }` — with the note that the
provider owns the exact declarations and *"consumers see only their stable identities after
selection."*

This is a semantic model of inlining, not a bytecode-level one. The project is already migrating off
pure constant-pool splicing, which is the single most native-hostile thing in the tree.

### The seam is test-enforced, not aspirational

`src/architecture.rs` (~800 lines of `#[cfg(test)]` guards) asserts module-dependency allowlists:
`src/backend.rs` may use only `diag`, `fir`, `frontend`, `ir`; `src/js/emit.rs` only `ir`,
`kt_string`, `types`; `src/ir_lower.rs` carries an explicit allowlist and is forbidden from
containing `fn resolve_` (`ir_lower_has_no_symbol_selection_entry_points`). **`src/ir_lower.rs` does
not import `crate::jvm` at all.**

### A second backend already proves the contract

`src/js/` (~1,300 lines) implements `Backend` and passes `EmptySymbolSource` as its platform — every
`TargetRuntime` method answering `None`. It covers roughly half the `IrExpr` variants and runs under
Node in `tests/js_backend_e2e.rs` and `tests/js_backend_coverage_e2e.rs`. Small, but existence proof
that a non-JVM backend compiles and runs through this contract.

### The project model, module graph, and caching precedent

`crates/krusty-lsp/src/project/` (~10,200 lines across 17 files) derives a `ProjectModel` of
`Module`s from Gradle (an injected init script emitting JSON), Maven, BSP, and JPS. `Module`
(`project/model.rs`) already carries `depends_on: Vec<ModuleId>`, `outputs`, `friend_paths`,
`source_roots`, `classpath`, `jvm_target`, `kotlinc_args` — the exact shape a scheduler needs. KMP
compilations are already extracted (`project/gradle.rs`, `kmp_module_of`).

Supporting precedent: `deps_cache.rs` (349 lines) is a versioned, globally locked, age- and
size-collected **content-addressed disk cache**; `project/fingerprint.rs` with `project/sync.rs`
content-hashes build files (wrapper, version catalogs, locks, `buildSrc`, `build-logic`) and skips
the build-tool probe when the fingerprint is unchanged; `src/lru.rs` with `src/jvm/classpath.rs`
memoizes classpath lookups with hit/miss counters under `KRUSTY_TRACE=cache`.

### Process orchestration

`crates/krusty-lsp/src/worker.rs` (~2,200 lines) is a restartable child-process worker with framed
JSON, a bounded launch frame, an analysis timeout, and `DEFAULT_ANALYSES_PER_WORKER = 64` to bound
interner lifetime. `crates/krusty-cli/src/worker.rs` (~1,200 lines) speaks the Bazel
persistent-worker protocol with an explicit `Refusal` enum so it fails loudly rather than emitting a
wrong jar.

### Multiplatform groundwork

The parser already accepts `expect` (`is_expect` on declarations in `src/ast.rs`, set in
`src/parser.rs`). The library set is already phrased platform-neutrally in the checker —
`src/resolve.rs` describes it as *"a JVM classpath or a klib"* and notes this eliminates *"the need
for any hardcoded type lists."* `docs/IMPLEMENTATION_PLAN.md` anticipates *"multiplatform: JVM
bytecode now, Kotlin/JS via klib later."*

---

## What does not exist

Stated plainly, because the sequencing depends on it.

**Build layer.**

- No ABI extraction and no compilation avoidance. `crates/krusty-cli/src/worker.rs` says it outright:
  krusty keeps no incremental state, and emits no reduced ABI jar, so *"a consumer that compiles
  against this one therefore rebuilds on any change, not only on ABI changes."*
- No output cache keyed on inputs, no module-DAG scheduler, no build daemon.
- The compilation unit is the whole module: the driver checks a source set, then streams files to
  the backend. There is no unit smaller than a module. For a Go-like model that is acceptable — Go's
  unit is the package — but it interacts badly with the next point.
- Frontend resolution is superlinear. `docs/LSP_INDEXING_PROFILE.md`: 1,000 files → 6.94s / 465 MiB;
  2,000 files → 22.47s / 1,270 MiB (2× files ⇒ 3.35× time, 2.73× memory), dominated by return
  pre-inference and signature collection, not parsing (3.3%). **Large single modules are the pain
  point that caching cannot hide.** (The FIR streaming work is expected to move these numbers;
  they should be re-measured before phase 4 sets any target.)
- All project-model code lives in the LSP crate, and `src/architecture.rs` forbids the compiler from
  depending on either process adapter. It must be lifted into a shared layer.

**Native target.**

- The common IR still carries JVM shapes: verbatim descriptors and dispatch kinds in `Callee`, the
  `$default` mask/marker ABI, `INSTANCE` singletons, `Ref$XxxRef` holders, a `java.lang.Class` ldc.
  `docs/COMPILER_REVIEW.md` §4, *"IR is partly backend-neutral and partly JVM bytecode IR"*, names
  this the top structural debt and proposes the common-IR / JVM-IR split.
- No klib reader or writer. The stdlib is mandatorily a jar (`stdlib_jar()` in `src/toolchain.rs`).
- No `expect`/`actual` *resolution* across a source-set hierarchy. Parsing exists; matching does not.
- No native code generator, and no runtime of any kind: no GC, object layout, vtables, exception
  mechanism, threading model, or coroutine scheduler. `src/jvm/suspend.rs` (~8,000 lines) shows the
  CPS transform is understood at IR level, but the scheduler beneath it is new.
- `docs/ARCHITECTURE.md` and `src/ir.rs` explicitly scope LLVM out of the *current* IR — *"LLVM is
  the right tool only for a future native backend (as in Kotlin/Native)."* This proposal agrees with
  that reading: the native path needs a second, lower IR beneath the current one, not a rerouting
  of it.

**Language coverage, which gates everything.** `docs/PROJECT_PARITY.md`: of 931 intellij-community
modules scanned, 4 check with zero errors. The build layer can ship far ahead of this (with
per-module fallback to `kotlinc`, mirroring the two modes of `bazel/defs.bzl`), but "blazing fast
builds of arbitrary real projects" ultimately gates on the existing language-surface grind, not on
anything in this document.

---

## Architecture

### Artifact model

Three artifact kinds per module, each independently cacheable:

| Artifact | Contents | Consumers |
|---|---|---|
| **ABI** | Declaration signatures only; no bodies except `inline` ones | Dependents' frontends; the cache key of every dependent |
| **Impl** | Target code — `.class` files today, native objects later | Link/package step only |
| **Metadata** | `@kotlin.Metadata` + `.kotlin_module` (JVM), klib metadata (native) | Downstream tooling, the LSP |

The ABI/Impl split is the whole game. A body-only edit changes Impl, leaves ABI byte-identical, and
therefore rebuilds no dependents. Two existing layers supply most of the content: `src/fir/header.rs`
and `src/fir/signature_extract.rs` compute the stable header inventory, and `src/metadata/`
(~4,600 lines) already writes a protobuf declaration model.

### Cache key

```
key(module) = H( compiler_version_and_flags
               ∥ sorted_source_content_hashes
               ∥ sorted ABI_hash(d) for d in direct_dependencies
               ∥ target_triple_or_jvm_target )
```

Dependency *ABI* hashes, never dependency source hashes — that is what makes avoidance transitive,
and what `crates/krusty-cli/src/worker.rs` records as missing today. Cache layout and eviction
follow `deps_cache.rs`: versioned root, global lock, age and size collection.

### Target contract for native

No new abstraction is introduced. A native target is:

- `KlibPlatform: SemanticPlatform + TargetRuntime` — reads declarations and inline bodies from klibs
  instead of `.class` files, and answers `TargetRuntime` with native ABI tokens rather than JVM
  descriptors. It satisfies `CompilerPlatform` through the existing blanket impl.
- `NativeBackend: Backend` — implements `lower_ir_file`, lowering checked common IR plus frozen
  classifier facts to a low native IR, then to machine code.

The `descriptor` fields in the IR are *provider-owned opaque tokens* by design: common lowering only
hands them back to the backend that issued them. A native platform may therefore mint its own token
grammar and work correctly *before* the IR split lands. The split remains required to make klib
*emission* faithful, and to stop `$default`/`INSTANCE`/JVM dispatch kinds from constraining native
lowering — but it does not block the first native experiment.

### Where the build layer lives

A new `crates/krusty-build` crate, depended on by both `krusty-cli` and `krusty-lsp`:

```
crates/krusty-build/
  model/        lifted from crates/krusty-lsp/src/project/ (gradle, maven, bsp, jps, detect, …)
  graph.rs      module DAG, cycle detection, topological scheduling
  cache.rs      content-addressed artifact store (deps_cache.rs lineage)
  abi.rs        ABI artifact extraction and hashing
  driver.rs     krusty build: plan → schedule → execute → report
```

The LSP keeps consuming the model exactly as today, so the lift is a move plus a dependency
inversion, not a rewrite. `src/architecture.rs` guards must be extended to allow the new edges and
to keep the compiler itself free of any dependency on the build crate.

---

## Phases

Sequenced so each ends green and is independently shippable. Per `CLAUDE.md`, every phase lands with
tests and a green `./run-tests.sh`.

### Phase 1 — Lift the project model into `krusty-build`

Move `crates/krusty-lsp/src/project/` into the new crate; `krusty-lsp` consumes it unchanged. Extend
the `src/architecture.rs` allowlists. **Ends green with the existing LSP project tests passing
against the new crate location, and no behavior change.** Pure refactor, no user-visible feature.

### Phase 2 — ABI artifact and hashing

Serialize a reduced ABI artifact per module (declarations plus `inline` bodies) from the FIR header
and signature-extraction layers, and hash it. Wire it to the Bazel worker's `--abi-out`, today a
declared-but-unwritten output.

*Test:* an ABI artifact is byte-stable across body-only edits; changing a signature changes the
hash; `javap` signatures of the ABI artifact match those of the full output. This is the
highest-leverage phase — everything downstream keys on it — and the one most helped by the FIR work.

### Phase 3 — Content-addressed output cache

Implement `cache.rs` and the key above. A second build of an unchanged module produces a cache hit
and no compiler invocation.

*Test:* hit/miss assertions over a synthetic multi-module fixture — untouched module hits; body edit
rebuilds that module only; signature edit rebuilds the module and its dependents; compiler-version
change invalidates everything.

### Phase 4 — `krusty build` driver and parallel scheduling

DAG scheduling across cores, with per-module fallback to `kotlinc` for modules krusty refuses (the
`Refusal` enum already distinguishes the cases). First phase with a user-visible command, and the
first that can be **benchmarked against Gradle + kotlinc on a real multi-module project** — that
benchmark is the deliverable, not just the code.

Phases 1–4 are target-independent and pay off on the JVM alone. **Stopping here is a coherent
outcome.**

### Phase 5 — klib ingestion

`KlibPlatform` implementing `SymbolSource` + `SemanticPlatform` over kotlinc-produced klibs, plus
`expect`/`actual` resolution across a source-set hierarchy. Enables checking `commonMain` with no
JVM present — immediately shippable through the LSP, independent of any native codegen.

*Test:* differential diagnostics against `kotlinc -Xmetadata-only` over common-source fixtures.

### Phase 6 — klib emission, then native codegen

Emit klibs, differential against kotlinc's, following the `docs/METADATA_NOTES.md` methodology.
krusty becomes a fast MPP *frontend* usable inside existing KMP builds before any native code exists.

Only then: a low native IR, a code generator, and the minimum runtime (allocation and a simple
collector, object layout and dispatch, exceptions, a coroutine scheduler) targeting Linux
x86-64/arm64, gated on the `codegen/box` corpus.

The IR split of `docs/COMPILER_REVIEW.md` §4 is a prerequisite for faithful klib *emission*, not for
ingestion. It is worth doing on its own merits for the JVM and JS targets regardless of whether
phases 5–6 ever start.

---

## Correctness strategy

The differential methodology is the project's foundation and transfers intact, with one relaxation
and one substitution.

| Phase | Oracle |
|---|---|
| 1 | Existing LSP project-model tests; no behavior change permitted |
| 2 | `javap` signature parity between ABI artifact and full output; byte-stability across body edits |
| 3 | Deterministic hit/miss assertions on a multi-module fixture |
| 4 | Whole-project output equivalence vs. a clean non-cached build; wall-clock benchmark vs. Gradle |
| 5 | Diagnostic parity vs. `kotlinc` metadata-only compilation of common sources |
| 6 | klib differential vs. kotlinc; then `codegen/box` **behavioral** parity vs. `kotlinc-native` |

**The relaxation:** native output is judged by `box()` returning `OK`, not by byte identity. The
invariant that carries over from `docs/PARITY_PROTOCOL.md` is the important one — *never miscompile
a case krusty accepts*; skipping is always permitted, silent wrongness never is.

**The substitution:** where the JVM target's oracle is `kotlinc`, the native target's is
`kotlinc-native` over the same corpus. An oracle exists; it is just a different binary.

---

## Risks and open questions

**The runtime is the real cost, and nothing in this repo de-risks it.** Phases 1–5 are extensions of
work krusty has already done well. Phase 6's runtime — GC, dispatch, exceptions, threading,
coroutine scheduling — has no precedent in the tree and is a multi-year item. It should not be
started until phases 1–4 have demonstrated the build-speed thesis on the JVM.

**A code generator conflicts with the dependency-lean ethos.** krusty has four runtime dependencies
(`zip`, `flate2`, `unicode-general-category`, `stacker`); it hand-writes its own class-file writer
and LRU, and `CLAUDE.md` forbids adding even a logging crate. Cranelift is the natural choice for
fast codegen — it is what `cg_clif` uses for exactly this reason, and it fits the thesis far better
than LLVM — but it is a large dependency tree, and adopting it is a **project-values decision that
must be made explicitly, not smuggled in under a phase.** The alternative, a hand-written code
generator as Go did, is defensible here but is a significantly larger commitment. *This is the
single most important open question in the document, and it is deliberately left open.*

**klib is a version-unstable format.** Its IR encoding changes between Kotlin releases. This is the
same hazard as `@kotlin.Metadata`, which the project already handles by pinning reference Kotlin
versions per release and reverse-engineering the schema into `docs/METADATA_NOTES.md`. Known hazard,
known playbook — but ongoing maintenance, not a one-time cost.

**Superlinear frontend cost limits the achievable win.** Caching removes repeated work; it does not
make a large module's first build fast. Single-module latency remains bounded by signature
collection and return pre-inference, work independent of this proposal. Re-measure after the FIR
streaming migration completes before setting any phase-4 target.

**The FIR migration is in flight.** Both `lower_file` and `lower_ir_file` exist on `Backend`. Phases
2 and 6 should build on the streaming IR path only, and should not begin in a way that would need
rework when the older path is retired.

**Open questions.**

1. Code generator: Cranelift, or hand-written? (See above — decide before phase 6, not during.)
2. Does the ABI artifact reuse the `.kotlin_module`/`@Metadata` protobuf model, or get its own
   format? Reuse is cheaper; a dedicated format hashes more stably. The FIR signature-extraction
   layer may make a third option — serializing its own records — cheapest of all.
3. Is a daemon needed at all? With instant process startup, the Go answer is no — and "no daemon" is
   itself a feature worth defending. Provisionally: no daemon; revisit only if phase-4 benchmarks
   show process startup is material.
4. Should `krusty build` ever own a native manifest format, or only ever read existing build models?
   Reading-only maximizes adoption; owning one enables the tightest cache keys.
