# Build system and native target — proposal

**Status: proposal.** Nothing here is implemented. This document scopes two related bets, records
the seams that already exist for them, and sequences the work so that every phase ends green and
ships value on its own.

The two bets:

1. **A Go-like build layer** — one binary that reads a module graph, hashes inputs, skips work whose
   ABI inputs are unchanged, and schedules the rest. Targets the JVM first, where krusty already
   emits correct artifacts.
2. **A Kotlin/Native-shaped target** — klib in, native code out, under the Kotlin Multiplatform
   contract (no Java interop).

They share a spine. Phases 0–5 below are prerequisites for both; only phases 6–8 are native-only.

References cite **files and symbols** rather than line numbers, which rot quickly here.

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

**Compact export data matters more than the cache.** Classpath probing is currently the dominant
compile cost: `docs/LAZY_CLASSPATH_RESOLUTION.md` measures `collect_signatures_with_cp` at **~65% of
compile**, memoized only per process (`src/lru.rs`, `src/jvm/classpath.rs`). Go's per-package export
data is what makes a process-per-unit build viable without a daemon. Replacing classpath probing
with purpose-built per-module export data is therefore the load-bearing win; compilation avoidance
is the second one.

**Kotlin/Native's release path is whole-program.** `-opt` binaries re-lower dependency klibs through
LLVM with full optimization at every link. Its klib compiler caches (since ~1.6/1.7,
`-Xauto-cache-from`, on by default for debug binaries) address the debug loop but not the release
one, and the first build of the stdlib and platform-library caches is itself expensive. The opening
is per-module cached native artifacts, not a faster instruction selector — LLVM-versus-Cranelift is
second-order, and the thesis above says why.

**Multiplatform removes the objection that killed native before.** Java interop is out of contract
for a native target — Kotlin/Native never had it; its interop is C and Objective-C/Swift. So a
native target does not forfeit krusty's identity. It adopts the same target contract JetBrains
already defined: klibs, `expect`/`actual`, the common stdlib.

**klib is a better library format than the classpath.** The compiler's deepest JVM coupling is that
the semantic model of the stdlib is read out of `.class` files, and that `inline fun` is realized by
splicing compiled bytecode — `src/jvm/inline.rs` relocates constant-pool indices between class
files, and `src/jvm/ir_emit.rs` calls `splice_unified` on the production streaming path. A klib
carries serialized Kotlin IR including inline function bodies, so a native path would replace
splicing with IR-level inlining. That is the architecturally cleaner design, but it does not exist
anywhere in the tree today (see [What does not exist](#what-does-not-exist)).

---

## Scope and non-goals

**In scope.** Deterministic emission; a total-output contract; a `krusty build` driver with a module
graph, ABI export data, and a content-addressed cache. Then klib ingestion and emission, and a
native code generator with the minimum runtime to run the `codegen/box` corpus on Linux
x86-64/arm64.

**Not in scope, deliberately.**

- *Java interop on the native target.* Out of contract. The JVM target keeps it; the native target
  never has it. `src/java_source.rs` and `src/jvm/java_stub.rs` stay JVM-only.
- *Byte-identical native binaries.* The JVM target's bar is byte parity with `kotlinc`. No such bar
  exists or should exist for native output; the oracle is behavioral.
- *Replacing Gradle.* The build layer reads existing Gradle/Maven/BSP/JPS models. A native manifest
  format is a later option, never an adoption prerequisite.
- *Objective-C/Swift export and cinterop.* Last, if ever.
- *A second general-purpose IR below the current one for the JVM target.* The native lowering gets
  its own low IR; the JVM path is not rerouted through it.

---

## What already exists

### A partial target contract

```
SymbolSource            src/symbol_source.rs     where declarations come from
  └─ SemanticPlatform   src/libraries.rs         library semantics in Kotlin terms
TargetRuntime           src/runtime.rs           platform ABI services (20 methods, 16 return None)
Backend                 src/backend.rs           lower_file / lower_ir_file / finalize → Vec<Artifact>
```

`src/jvm/jvm_libraries.rs` (~9,000 lines) implements `SymbolSource`, `SemanticPlatform` and
`TargetRuntime`; `src/jvm/backend.rs` implements `Backend`.

**`CompilerPlatform` is not the contract.** `src/runtime.rs` declares
`pub trait CompilerPlatform: SemanticPlatform + TargetRuntime {}` with a blanket impl, but those two
lines are its *only* occurrences in the repository — nothing is bounded on it, nothing stores one.
The real seam takes the halves separately: the frontend takes `Box<dyn SemanticPlatform>`
(`src/frontend.rs`), and the backend constructs its own runtime (`JvmBackend::new(cp)`).
`crates/krusty-lsp/src/worker.rs` records the friction directly — analysis holds a
`Box<dyn SemanticPlatform>` *"which cannot be re-borrowed as a `TargetRuntime`."* A new target
implements the two halves; unifying them is unfinished work, not an available abstraction.

**`TargetRuntime` is genuinely target-neutral where it reaches.** `PlatformCtor`, `PlatformAccessor`,
`RangeConstruction`, `RuntimeOp` and `mutable_local_ref_type` keep platform names out of their
callers. But see the next point for how far that reach actually goes.

### The backend boundary is a streaming IR boundary

`Backend` carries `lower_ir_file(CheckedIrFile)`, and the shipped CLI takes that path
(`crates/krusty-cli/src/main.rs` → `compiler::emit_analyzed` → `lower_ir_file`). Its contract reads:
*"No parsed source or AST-keyed semantic table crosses this boundary; target realization consumes
only checked IR and compact stable module facts."* `src/backend/module_facts.rs` adds
`BackendClassifierSource` — *"the only semantic query a representation backend may make after common
lowering"* — returning frozen `BackendClassifierFact` records free of `Ty::Pending` and `Ty::Error`.

The legacy `lower_file(CheckedFile)` path has **no production caller**: `compiler::emit_checked` is
reached only from tests, `src/dump.rs`, and `src/bin/*`. This is a completed migration with a
vestigial second entry point, not a migration in flight.

**The boundary is narrow but not serializable.** `BackendClassifierSource` is a lazy query trait, and
its production impl wraps a live `&dyn SymbolSource` — the classpath. `JvmBackend` separately holds
`Rc<Classpath>` and reads it during `lower_ir_file` (range, function-reference and property-reference
realization) and again at emit time for inline splicing. `IrFile` also carries `Ty`/`TypeName`
interned as process-lifetime `Box::leak`ed values. So the declared input is narrow and stable; the
*actual* input includes the classpath, and nothing here is serializable today. It is a good
foundation for a serializable module input, not one already built.

### The frontend separates a header inventory from body checking

`src/fir/` (~64,000 lines; ~39,000 excluding tests) is a *"streaming frontend ownership model"* whose
module tree *"follows frontend lifetime boundaries: stable header inventory, temporary signature
solving, checked body ownership, and exhaustive parser coverage."*

This is the right shape for an ABI artifact but is **not** one yet. `src/fir/header.rs` states its
own scope: it contains *"only data structures"*, and *"signature evaluation deliberately does not
live here."* `StreamedHeaderModule` holds packed, *unresolved* type syntax and module-local ids whose
`signature_origins` are destroyed on consumption. An ABI artifact needs resolved signature types,
which come from the layer `src/fir/mod.rs` calls *"temporary signature solving."*

### `expect`/`actual` matching exists

`src/fir/header.rs::actualized_declaration_pairs` matches `expect` to `actual` over compact headers:
name/kind/arity keys, type-flag matching, recursive type-shape matching with type-parameter renaming
and actualized-typealias awareness. What is missing is matching across a *source-set hierarchy*.

### Project model, module graph, and caching precedent

`crates/krusty-lsp/src/project/` (~10,200 lines, 17 files) derives a `ProjectModel` from Gradle (an
injected init script emitting JSON), Maven, BSP and JPS. `Module` carries `depends_on`, `outputs`,
`friend_paths`, `source_roots`, `classpath`, `jvm_target`, `kotlinc_args`. KMP compilations are
extracted (`project/gradle.rs::kmp_module_of`).

`crates/krusty-lsp/src/deps_cache.rs` (349 lines) is a versioned, globally locked, age- and
size-collected content-addressed cache — of *browsable dependency sources* for LSP navigation, not
build artifacts. It is a design precedent to copy, not a component to reuse.
`project/fingerprint.rs` and `project/sync.rs` content-hash build files and skip the build-tool probe
when unchanged.

### Process orchestration

`crates/krusty-lsp/src/worker.rs` (~2,200 lines) is a restartable child-process worker with framed
JSON and `DEFAULT_ANALYSES_PER_WORKER = 64`, bounding interner lifetime.
`crates/krusty-cli/src/worker.rs` (~1,200 lines) speaks the Bazel persistent-worker protocol and
keeps a decoded classpath warm across requests.

### A second backend proves the `Backend` trait

`src/js/` (~1,300 lines) implements `Backend` and handles 26 of 54 `IrExpr` variants, running under
Node in two e2e test files. It is generic over `TargetRuntime`, with `EmptySymbolSource` supplied by
test harnesses; there is no production JS driver.

---

## What does not exist

### Emission is not deterministic — and this blocks everything

Two independent measurements, in two documents:

- `docs/RESOLUTION_ENGINE_PLAN.md`: *"the BASE binary produces four distinct md5s for
  `unqualifiedSuperKt$box$1.class` across six runs of the same input. Emission order is keyed by hash
  iteration order over the class signature's member maps, which is a separate pre-existing defect."*
- `docs/SPEC.md`: *"A 47th differs between any two runs of the SAME binary — a pre-existing
  non-deterministic emission, not a guard."*

**A content-addressed cache over a nondeterministic producer inverts.** An unchanged module's ABI
bytes change on rebuild, so its hash changes, so every dependent rebuilds. Worse, caching *hides* the
defect: a nondeterminism bug that would surface as a byte diff instead surfaces as a cache hit. And
Phase 4's natural oracle — output equivalence against a clean rebuild — is unwritable while a clean
build does not equal a second clean build. Hence Phase 0.

### The compiler silently emits partial output

`src/compiler.rs` skips a source that hits an unsupported shape (`if source_rejected { continue; }`)
with no error diagnostic; the CLI then reports success and exits 0. For a conformance corpus this is
the correct `docs/PARITY_PROTOCOL.md` posture — a skip means "this case does not count." In a build
system a skip means a jar is missing classes, and the failure surfaces at link or run time in a
different module.

Caching would make that permanent, storing a truncated artifact under a key asserting it is correct.
The Bazel worker's `Refusal` enum does not cover this: its three variants (`JavaSources`,
`Unsupported`, `Malformed`) are all decided from the work request's flags *before* compilation, while
the refusals that matter are per-file and discovered mid-lowering.

### Build layer

- **No ABI extraction.** The Bazel worker's `--abi-out` is written, but as a byte copy of the full
  jar — the source comments say so directly: *"krusty has nothing distinct to put in them"*, and a
  consumer therefore *"rebuilds on any change, not only on ABI changes."* No reduction exists.
- No output cache, no module-DAG scheduler, no build daemon, no `krusty build`, no `krusty-build`
  crate.
- The compilation unit is the whole module; there is no smaller unit.
- **The compiler is not `Send`.** Compiler state deliberately holds `Rc`/`RefCell` (hence the
  `stacker` dependency), and interning leaks — which is why the LSP restarts its worker every 64
  analyses. Parallelism must be process-per-module or a restarted worker pool, not threads.
- Frontend cost is superlinear, and the current figures are worse than the widely-quoted ones.
  `docs/LSP_INDEXING_PROFILE.md` records 1,000 files → 6.94s/465 MiB and 2,000 → 22.47s/1,270 MiB on
  its profiling base, but then re-measures on `acee6cd0` at **1,000 → 12.52s/260 MiB and 2,000 →
  50.03s/415 MiB** (4.0× time, 1.6× memory), and warns that even those must be re-measured against
  present-day `master`. Caching cannot hide single-module latency.
- Project-model code lives in the LSP crate and `src/architecture.rs` forbids the compiler depending
  on either process adapter.

### Native target

- **The common IR commits to JVM semantics, not merely JVM spellings.** `IrExpr::StaticInstance`
  is constructed with the literal `"INSTANCE"` at ~7 sites in `src/ir_lower.rs`, asserting eager
  static-field singleton initialization (Kotlin/Native initializes objects lazily with thread-state
  checks). `Callee::LocalDefault`/`ClassStaticDefault` encode the `(…, int mask, Object marker)`
  `$default` convention; `Callee::Virtual`/`Special` carry `interface: bool` for JVM dispatch
  selection; `IrExpr::ClassConst` is defined as a `java.lang.Class`. Common lowering also *parses*
  descriptors via `descriptor_method_layout` to locate continuation slots and to decline a file on
  reference/primitive slot mismatch. `docs/COMPILER_REVIEW.md` §4 states it plainly:
  *"`ir_lower.rs` builds descriptors and JVM owners before the backend gets control."*
- **`TargetRuntime` does not reach the production lowering path.** `src/fir_lower/` — the lowering
  the shipped driver uses — contains **zero** references to `TargetRuntime` or `crate::runtime`, and
  `src/architecture.rs` does not permit it that dependency. `TargetRuntime` is threaded only through
  the legacy `src/ir_lower.rs` (as `&dyn`, not generically). A native target needs that abstraction
  extended to `fir_lower`, which does not exist yet.
- **`inline` is still bytecode splicing, including its "semantic" model.** `InlineBodyPlan`
  (`src/libraries.rs`) has exactly three variants, and they are produced by *disassembling the
  stdlib's compiled body*: `jvm_libraries.rs::inline_body_plan_uncached` reads `<name>$$forInline`
  or `<name>` `method_code` from the jar and calls `crate::jvm::inline::disassemble`. Coverage is
  narrow and partly hardcoded — `CollectionTransform` is attached to two intrinsics (`Map`,
  `FlatMap`) over a literal `java/util/ArrayList` table; `InvokeLambda` matches only bodies with
  exactly one function invoke. General `inline fun` expansion remains constant-pool splicing on the
  production path. This is a JVM-provider-side decode of bytecode into a neutral plan — evidence that
  a neutral description is *possible*, not evidence of a migration under way.
- No klib reader or writer; no `expect`/`actual` resolution across a source-set hierarchy; no native
  codegen; no runtime of any kind (no GC, object layout, dispatch, exceptions, threading, or
  coroutine scheduler). `src/jvm/suspend.rs` (~8,000 lines) is a JVM-only IR→IR CPS transform.
- `docs/ARCHITECTURE.md` scopes LLVM out of the current IR: *"LLVM is the right tool only for a
  future native backend (as in Kotlin/Native)."* This proposal agrees — the native path needs a
  second, lower IR beneath the current one.

### Language coverage gates the end-user claim

`docs/PROJECT_PARITY.md`: of 931 intellij-community modules scanned, **4** check with zero errors,
916 report errors, 10 crash (SIGBUS), 1 times out. A build layer can ship ahead of this, but a
whole-project benchmark today measures an orchestrator around `kotlinc`, and the scheduler needs
crash isolation with a defined per-module outcome.

---

## Architecture

### Artifact model

| Artifact | Contents | Consumers |
|---|---|---|
| **ABI** | Class headers, `@kotlin.Metadata`, constant values, non-`SOURCE` annotations, sealed-subclass lists, enum entry order, contracts, **compiled bodies of `inline` functions** plus the bridges and synthetics they reach, and `.kotlin_module` | Dependents' frontends; every dependent's cache key |
| **Impl** | Everything else — non-inline method bodies, suspend state machines | Link/package step only |

On the JVM, `@kotlin.Metadata` *is* the ABI — it is what `src/jvm/jvm_libraries.rs` reads to check
against a dependency — so it belongs in the ABI artifact, not beside it.

**Four things put bodies in the ABI**, which the naive "signatures only" reading gets wrong and each
of which is a silent-miscompile risk:

1. **`inline` bodies.** On the JVM these are *compiled bytecode* read from the dependency's class
   files. So the ABI artifact is a reduced **class-file set**, not a serialized FIR record set, and
   producing it requires the backend — not the frontend alone.
2. **`const val` initializer values.** Folded at the consumer: `src/jvm/classreader.rs` reads a
   `static final` field's `ConstantValue` attribute, `src/jvm/jvm_libraries.rs` turns it into a
   `LibraryConst`, and `src/fir/body_check.rs` publishes it as a `FirExprKind::Constant` on the read.
   Changing `30` to `60` with no signature change must therefore rebuild dependents. (krusty declines
   one narrow case rather than modelling it — an `object` whose `init` block has side effects, where
   a `const val` read must not trigger initialization.)
3. **Contracts.** `src/contracts.rs` flows a `contract { … }` block into `Function.contract` and into
   callers' smart-cast analysis — a construct that lives syntactically inside a body.
4. **Default argument expressions** for `inline` functions, annotation classes, and `@JvmOverloads`.

Also in the ABI because they change the physical shape: `@JvmName`, `@JvmStatic`, `@JvmField`,
`@JvmOverloads`, `@JvmInline`, `@PublishedApi`, `@Deprecated(HIDDEN)`, `@InlineOnly`, `@Throws`;
value-class carrier and mangling; `suspend`-ness; `inline`/`reified`; data-class `componentN` order;
`@JvmDefault` mode. Excluded: `AnnotationRetention.SOURCE` annotations, or every `@Suppress` edit
cascades.

**`internal` needs two hashes.** Friendship is path-set membership against classpath entries. Pruning
`internal` declarations breaks friend modules (every `test` source set); keeping them invalidates
non-friend dependents on every `internal` edit. Emit `abi_hash_public` and `abi_hash_friend`, and let
each edge select one.

**Suspend is where the split is cleanest:** the `Continuation` parameter and erased return are ABI;
the state machine built in `src/jvm/suspend.rs` is Impl.

### Cache key

A wrong cache hit ships a wrong binary with no diagnostic — the highest-severity failure mode in this
design. Every input below is in the key by default; an input leaves the key only with a test
demonstrating output invariance under its change.

```
key(module) = H( krusty_version_and_build_id
               ∥ canonical_compiler_flags            # incl. -module-name, -jvm-target, -Xjvm-default,
                                                     #  friend paths, assertions, opt-ins, language version
               ∥ output_affecting_environment        # explicit allowlist: JAVA_HOME identity,
                                                     #  KRUSTY_NO_CLASS_METADATA, KRUSTY_LANGUAGE_VERSION
               ∥ jdk_identity                        # java.version + content hash of lib/modules or ct.sym
               ∥ ordered [(source_relpath, content_hash)]
               ∥ ordered [(classpath_entry_path, content_hash)]
               ∥ ordered [(friend_path, content_hash)]
               ∥ ordered ABI_hash(d) for d in direct_dependencies
               ∥ plugin_set                          # each plugin jar's content hash + its options
               ∥ target_triple_or_jvm_target )
```

`ordered`, not `sorted`, and content hashes rather than paths or mtimes. Each deviation from the
obvious key is forced by something in the tree:

- **Source order and basenames are load-bearing.** `JvmState::module_packages` accumulates facade
  names in file-streaming order and emits them unsorted; the CLI derives `stems` from
  `file_stem(path)`, which names the facade class and the `SourceFile` attribute.
- **Classpath entry *paths* are semantically load-bearing**, not just their contents:
  `src/plugins/serialization.rs` parses the *jar file name* to choose `write$Self` versus
  `write$Self$<module>` mangling.
- **`module_name` reaches the bytes** three ways: `@Metadata.classModuleName`, the
  `META-INF/<module>.kotlin_module` file name, and that serialization mangle.
- **`friend_paths` are path-identity-based** and change which `internal` declarations are visible.
- **Environment reaches the bytes**: `KRUSTY_NO_CLASS_METADATA` switches off per-class `@Metadata`
  outright; `KRUSTY_LANGUAGE_VERSION` selects the reference version; `JAVA_HOME` selects the
  bootclasspath, which determines what `java.*` resolves to.
- **KSP processors are external jars that generate sources** (`src/plugins/ksp.rs` runs a fixpoint
  through `src/plugins/codegen_loop.rs`), and `KspToolchain` is build-resolved, so the build layer is
  exactly where processor identity must be hashed.

The tree's existing entry identity (`classpath.rs::path_identity`, using dev/ino/ctime/mtime) is fine
for an in-process memo but unusable as a build-cache key: a `git checkout` restoring identical bytes
changes ctime.

**In-process staleness.** `src/jvm/classpath.rs` memoizes scans per entry. A long-lived worker that
compiles module B after writing module A into a directory on B's classpath can serve a stale scan.
Either invalidate on write, or use a fresh worker for any module whose classpath contains a
just-produced output.

### Target contract for native

A native target implements `SemanticPlatform` + `TargetRuntime` (`KlibPlatform`) and `Backend`
(`NativeBackend`, via `lower_ir_file`). Two things must land first, and neither is optional:

1. **`TargetRuntime` must reach `fir_lower`**, which today has no access to it.
2. **The IR split** (`docs/COMPILER_REVIEW.md` §4, Step 3 of its refactor order) must remove
   `"INSTANCE"`, the `$default` convention, `interface: bool` dispatch and `ClassConst` from common
   lowering. These encode JVM *semantics* a native target cannot reinterpret by minting different
   token strings.

Without both, a native backend is limited to a subset with no objects, no default arguments, no
suspend and no class literals — a useful spike, not a path to corpus coverage.

### Where the build layer lives

A new `crates/krusty-build` crate, consumed by both `krusty-cli` and `krusty-lsp`:

```
crates/krusty-build/
  model/        lifted and EXTENDED from crates/krusty-lsp/src/project/
  graph.rs      module DAG, cycle detection, topological scheduling
  cache.rs      content-addressed artifact store (deps_cache.rs lineage)
  abi.rs        ABI artifact extraction and hashing
  driver.rs     krusty build: plan → schedule → execute → report
```

This is **not** a pure refactor. The LSP model is deliberately not a build model: it omits
**resources** (`project/jps.rs`: *"resource folders skipped"*), **module name** (which reaches the
bytes), **annotation-processor/KSP configuration**, **Java sources** (Gradle modules routinely mix
Java; krusty has no Java frontend), per-module JDK home, and output kind (jar versus class dir). It
also currently feeds the worker *"the union of all module classpaths"* — the opposite of per-module
isolation.

**A dependency-policy decision is required here, not later.** `krusty-lsp` pulls `serde`,
`serde_json`, `roxmltree`, `url`, `fs2`, `sha2`; making `krusty-cli` depend on a lifted model would
add `roxmltree`, `url`, `fs2`, `sha2` to the shipped compiler binary. `docs/ARCHITECTURE.md`
quarantines exactly these: *"`serde`, `serde_json`, JSON-RPC transport, and session state belong to
the separate `crates/krusty-lsp` workspace package."* This deserves the same explicit decision as
Cranelift.

---

## Phases

Each ends green with tests, per `CLAUDE.md`.

### Phase 0 — Deterministic emission

Nothing downstream is meaningful without it (see [What does not exist](#what-does-not-exist)).
Replace every unordered collection on an emission-ordering path with an ordered container or an
explicit sort at the boundary, and add an `src/architecture.rs`-style guard forbidding unordered
iteration in `src/jvm/ir_emit.rs`, `src/jvm/classfile.rs`, `src/metadata/`, and
`JvmBackend::finalize`.

*Test:* N-run byte identity over the full `codegen/box` corpus, as a gate rather than a one-off
measurement. The sweep harness from `docs/RESOLUTION_ENGINE_PLAN.md` already exists and already made
this measurement. Zero classes may differ across runs.

### Phase 1 — Total-output contract

Make every declined declaration an observable, module-level failure: `source_rejected` must fail the
module with a distinct exit status the build layer can route, rather than exiting 0 with a short jar.
This matches the Bazel worker's existing posture of failing loudly rather than emitting a wrong jar.

*Test:* a module containing an unsupported construct exits non-zero and emits no partial artifact.

### Phase 2 — Lift and extend the project model into `krusty-build`

Move `crates/krusty-lsp/src/project/`; extend it with resources, module name, processor
configuration, Java-source detection, per-module JDK and output kind; resolve the dependency-policy
question above. `krusty-lsp` consumes the result unchanged.

*Test:* existing LSP project tests pass against the new crate; new fixtures cover each added field.

### Phase 3 — ABI artifact and hashing

Emit a reduced ABI artifact per module with the contents listed in
[Artifact model](#artifact-model). Declarations come from the FIR header and signature layers; inline
bodies and bridges must come from the JVM backend, so **this phase spans frontend and backend** and
is larger than serialization. Replace the Bazel worker's full-jar copy at `--abi-out`.

*Test:* (a) ABI bytes stable across body-only edits of non-inline, non-`const`, contract-free
declarations; (b) a signature change, a `const val` value change, a contract change, a new sealed
subclass, or an `inline` body change each changes the hash; (c) **round-trip** — compiling a
dependent against the ABI artifact produces byte-identical output to compiling it against the full
module output. (c) is the oracle that matters.

Measure this phase against classpath probing (~65% of compile) as well as against avoidance; that
number is what decides whether a daemon is needed.

### Phase 4 — `krusty build` driver and module DAG, sequential

Plan, order and execute a module graph with no cache and no parallelism. Define the process model
(process-per-module or a restarted worker pool — the compiler is not `Send`), crash isolation with a
defined outcome per module, and diagnostic reporting across modules.

*Test:* whole-project output equals a hand-run sequence of `krusty` invocations. This assertion
depends on Phase 0.

### Phase 5 — Content-addressed cache and parallel scheduling

Implement the corrected key. Then parallelize.

*Test:* untouched module hits; body edit rebuilds that module only; signature or `const` edit
rebuilds the module and its dependents; a change to any keyed input invalidates. Benchmark against a
*warm* Gradle daemon with configuration cache, build cache and kotlinc incremental all enabled —
anything less measures Gradle's cold start, which nobody disputes. State plainly what share of
modules fall back to `kotlinc`.

Phases 0–5 are target-independent. **Stopping here is a coherent outcome.**

### Phase 6 — IR split

`docs/COMPILER_REVIEW.md` §4, plus extending `TargetRuntime` to reach `fir_lower`. A prerequisite for
native codegen *and* for faithful klib emission, and worth doing for the JVM and JS targets
regardless of whether anything below it happens.

### Phase 7 — klib ingestion

`KlibPlatform` over kotlinc-produced klibs, plus `expect`/`actual` across a source-set hierarchy —
building on `actualized_declaration_pairs`, which already matches within a module. Note that a KMP
source-set hierarchy is a `dependsOn` graph among *source sets*, where `commonMain` compiles *into*
each target's compilation; `Module::depends_on` models compile-classpath edges. `graph.rs` must say
whether it models one graph or two. Also note that the Gradle init script and the Rust parser
currently *filter out* non-JVM compilations and `.klib` files, so Phase 7 partly undoes Phase 2.

*Test:* diagnostic parity against `kotlinc -Xmetadata-only` over common-source fixtures.

### Phase 8+ — klib emission, then native

Separate tracks, explicitly outside the phase discipline: klib emission (differential against
kotlinc's); then a low native IR; then a code generator; then a runtime. The runtime alone is
allocation and collection, object layout and dispatch, exceptions, threading and a coroutine
scheduler.

---

## Correctness strategy

| Phase | Oracle |
|---|---|
| 0 | N-run byte identity over the `codegen/box` corpus, gated |
| 1 | A module with an unsupported construct fails; no partial artifact is produced |
| 2 | Existing LSP project-model tests; new fixtures per added field |
| 3 | Round-trip: compiling against the ABI artifact equals compiling against full output |
| 4 | Whole-project output equals a hand-run invocation sequence |
| 5 | Hit/miss assertions per keyed input; benchmark vs. fully warm Gradle |
| 6 | Existing differential harness stays green through the split |
| 7 | Diagnostic parity vs. `kotlinc -Xmetadata-only` |
| 8+ | klib differential vs. kotlinc; `codegen/box` **behavioral** parity vs. `kotlinc-native` |

**The relaxation:** native output is judged by `box()` returning `OK`, not byte identity. The
`docs/PARITY_PROTOCOL.md` invariant carries over unchanged — never miscompile a case krusty accepts;
skipping is permitted, silent wrongness never is. Note Phase 1 sharpens what "skip" may mean in a
build.

**The substitution:** `kotlinc-native` replaces `kotlinc` as the native oracle.

---

## Risks and open questions

**The runtime is the real cost.** Phases 0–7 extend work krusty has done well. The Phase 8 runtime
has no precedent in the tree and is a multi-year item. It should not start until Phases 0–5 have
demonstrated the build-speed thesis on the JVM.

**Deferring the code generator is right; deferring the low IR's shape is not.** Three decisions shape
the low native IR and must be answered when it is designed, independently of Cranelift-versus-
hand-written: (1) precise versus conservative GC — precise requires explicit safepoint and stack-map
nodes in the IR; (2) exception propagation — table-driven unwinding versus explicit result
propagation; (3) whether `suspend` lowers through the existing CPS transform or to native stack
switching.

**The code-generator dependency is a project-values decision.** The compiler *library* has four
runtime dependencies (`zip`, `flate2`, `unicode-general-category`, `stacker`), hand-writes its own
class-file writer and LRU, and `CLAUDE.md` forbids adding even a logging crate. (The shipped binary
already adds `serde`, `serde_json`, `zip` via `krusty-cli`.) Cranelift is the natural fast-codegen
choice, but `cg_clif`'s reported wins over LLVM are roughly 1.3–1.5× on debug builds — not an order
of magnitude, and not by itself the thesis. Decide explicitly, before Phase 8, never inside a phase.
The same standard applies to `roxmltree`/`url`/`fs2`/`sha2` in Phase 2.

**klib is a version-unstable format.** Its IR encoding changes between Kotlin releases — the same
hazard as `@kotlin.Metadata`, handled the same way (pin reference versions, reverse-engineer into
`docs/METADATA_NOTES.md`), but as ongoing maintenance.

**Superlinear frontend cost bounds the win.** Caching removes repeated work, not a large module's
first build. Re-measure `docs/LSP_INDEXING_PROFILE.md`'s figures against present-day `master` before
setting any Phase 5 target; that document already warns its own numbers are stale.

**Open questions.**

1. Code generator: Cranelift, or hand-written?
2. Does the ABI artifact reuse the `@Metadata` protobuf model or get its own format? Reuse is
   cheaper and is already what dependents read; a dedicated format hashes more stably.
3. Is a daemon needed? The answer depends on Phase 3: with compact export data replacing classpath
   probing, process-per-module is viable; without it, every module pays ~65% of a compile on
   classpath decoding and a persistent worker becomes mandatory.
4. Remote/shared caching, cache poisoning and trust, and concurrent builds against one cache are
   unaddressed here and need their own design before the cache is shared beyond one machine.
5. Windows and macOS support for the build layer is unscoped.
