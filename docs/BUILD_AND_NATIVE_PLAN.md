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

### Emission determinism is unproven, not known-good

Two historical measurements record nondeterministic emission:

- `docs/RESOLUTION_ENGINE_PLAN.md`: *"the BASE binary produces four distinct md5s for
  `unqualifiedSuperKt$box$1.class` across six runs of the same input. Emission order is keyed by hash
  iteration order over the class signature's member maps, which is a separate pre-existing defect."*
- `docs/SPEC.md`: *"A 47th differs between any two runs of the SAME binary — a pre-existing
  non-deterministic emission, not a guard."*

**Neither reproduces on `master` at 00dd62d.** Measured while writing this document: the exact class
named in the first report (`unqualifiedSuperKt$box$1.class`, from an unqualified-super fixture with
an object expression) is byte-identical across 12 runs, and a generated 113-class output — 40
interfaces, 120 members, 30 object expressions — is byte-identical across 10 runs, whole-output
digest included. Both reports predate the FIR streaming migration; the defect appears to have been
fixed incidentally.

So the property currently holds for the shapes tested and has never been **swept, gated, or
protected**. That is the actual gap: a content-addressed cache over a nondeterministic producer
*inverts* — an unchanged module's hash changes, so every dependent rebuilds — and caching then
*hides* the defect, because a nondeterminism bug that would surface as a byte diff instead surfaces
as a cache hit. Phase 4's oracle, output equivalence against a clean rebuild, is also unwritable
without it. Phase 0 therefore establishes a gate rather than performing a fix.

**Emitted output is source-order sensitive, and that is not a defect.** Also measured: compiling four
same-package files in reverse order changes `.kotlin_module` bytes, because a package's facade-name
list accumulates in file-streaming order (`JvmState::module_packages`). `DeltaKt EchoKt FoxtrotKt
ZuluKt` becomes `ZuluKt FoxtrotKt EchoKt DeltaKt`. Class files themselves are unaffected. This is
input-order sensitivity rather than nondeterminism, and it is why the cache key below hashes an
**ordered** source list; a key over a sorted multiset would collide two builds with different
`.kotlin_module` output.

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
(`CraneliftBackend`, via `lower_ir_file`). Two things must land first, and neither is optional:

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

### Phase 0 — Gate emission determinism

The property holds for the shapes measured (see
[What does not exist](#what-does-not-exist)) but is unprotected, so the work is a gate, not a fix.
Add an N-run byte-identity test, then widen it to a `codegen/box` corpus sweep using the harness from
`docs/RESOLUTION_ENGINE_PLAN.md`. Pin the source-order sensitivity of `.kotlin_module` in the same
test file, since the cache key depends on it. Should the sweep find surviving nondeterminism, the fix
is an ordered container or an explicit sort at the boundary, plus an `src/architecture.rs`-style
guard forbidding unordered iteration in `src/jvm/ir_emit.rs`, `src/jvm/classfile.rs`,
`src/metadata/`, and `JvmBackend::finalize`.

*Test:* zero classes differ across N in-process compilations of the same source; `.kotlin_module`
facade order tracks source order. Both land as a regression gate.

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

#### Requirement: every target buildable from any host

Cross-compilation is not a later convenience. Go's defining build property is that
`GOOS=linux GOARCH=arm64 go build` works on any machine with nothing else installed, and a native
target that needs a toolchain per architecture has given up the thing worth copying. This is
therefore a requirement on the native track, at the same level as build speed.

What makes cross-compiling C painful is not the compiler — `clang` targets every architecture it
was built with, and `ld.lld` links all of them — it is the **sysroot**: target headers and a target
libc. So the requirement is really a constraint on the *runtime*: it must not use a C library. The
landed runtime therefore talks to the kernel directly (write, mmap, exit_group) through a syscall
shim per architecture, uses only the compiler-provided freestanding headers, and supplies its own
`_start`. One host then builds every supported target with no sysroot, and the test asserts the
produced binary's ELF machine number rather than trusting that it cross-compiled.

Supported today: `linux-x86_64`, `linux-aarch64`, `linux-riscv64`. Adding an architecture is a
register convention and two syscall numbers, not a port — that is the whole point of keeping
everything above the shim portable C. Adding an *operating system* is a real port, because the
syscall interface is the part that is not portable: macOS and Windows do not have a stable one, so
those targets need either a libc (and its sysroot) or a different strategy entirely.

#### Landed, then retired: a native backend that emits C

*Historical.* The C emitter described here was the scaffold; it is deleted now that every test
written against it runs through krusty's own code generator (see *Decided: krusty owns the code
generator too*). The runtime and object model it was built around are what survived.

`src/native/` compiles checked common IR to C and links it with `cc`. `fun main() { println(…) }`
builds to an executable that runs with `JAVA_HOME` and `PATH` emptied, and so do arithmetic, locals,
`while`, a lowered `for`, recursion, string concatenation and string templates. (Its tests have
since moved to `tests/native_codegen_e2e.rs`, where the same programs run through the owned code
generator; the C path keeps only the class and collector tests until those migrate.)

**C, not Cranelift, and not LLVM.** This document's whole argument is that the code-generator choice
deserves a decision rather than a default, and printing `Hello, world!` through either candidate
would have made that decision by accident. Emitting C decides nothing: the same lowering feeds a
real backend when the choice is made, and in the meantime `cc` is already present wherever this
compiler builds. The emitted program links against the generated `krusty_rt.c` and `krusty_gc.c` and nothing else — no C library —
a native binary, produced without committing the project to anything.

What this does NOT do, in order of how much it matters:

* **Almost all of the stdlib.** Each declaration the runtime does not implement makes the backend
  decline, by name, with a diagnostic. The runtime implements `kotlin.io`'s console functions,
  `String.plus`, and `kotlin.Any`'s `toString`/`hashCode`/`equals`.
* **Interfaces, data classes, enums, `inner` classes, secondary constructors, constructor
  defaults, closures, exceptions, collections, threads, and a class used from another file of the
  module.** Classes with single inheritance, virtual dispatch, `is`/`as` and `object` declarations
  run (see *Decided: krusty owns its runtime* below); each item on this list still declines by
  name.
* **klib ingestion (phase 7).** Symbols still come from the Kotlin/JVM stdlib jar, since that is the
  only provider krusty has. Only *signatures* come from there — no JVM reaches the output — but two
  seams exist because of it and both disappear with phase 7: a top-level function arrives owned by a
  JVM file facade (`kotlin/io/ConsoleKt`), and `kotlin.String` arrives spelled `java/lang/String`.
  Both are normalized in one place (`src/native/intrinsics.rs`) so nothing else is written in JVM
  spellings.
* **Anything about speed.** No measurement is claimed. `-O0` is passed to `cc` deliberately — a
  target whose premise is that build time is the scarce resource has no business asking the C
  compiler to spend the time it exists to save — but the thesis stands unmeasured until there is
  enough of a language to measure.

The backend is not wired into the CLI: it is reachable from the library API and exercised by tests.
Exposing a `--target native` flag would advertise a language subset this small as a target.

#### Open: emit Go instead of C?

Worth taking seriously, and measured rather than argued. "Reuse Go's backend" has exactly one
workable form — **emit Go source and run `go build`**. The other two do not work: `cmd/compile`'s SSA
backend is not a library another language can drive, and `-buildmode=c-archive` gives Go's collector
only over Go-allocated objects, so a Kotlin heap stays invisible to it.

Measured on one machine, the same hello world, cross-built from `linux/amd64`:

| | C, freestanding | Go |
|---|---|---|
| cross-build, warm | 0.18–0.20 s | 0.17–0.21 s |
| cross-build, first time for a target | 0.18–0.20 s | ~5 s (one-off: compiles that target's stdlib) |
| binary size | ~10 KB | ~1.9–2.2 MB |
| targets from one host | 3, each needing a hand-written syscall shim | 48 `GOOS/GOARCH` pairs, including macOS and Windows |
| toolchain needed | clang + lld | go |

The build-speed numbers are the surprise: once a target's stdlib is cached, Go costs nothing
measurable. Since the thesis already accepts larger binaries, size is the weaker objection — and
against it Go supplies, at no implementation cost, precisely what this document calls the
multi-year item: a precise generational GC, growable stacks, goroutines and channels (a plausible
`suspend` realization that would retire the CPS transform), `panic`/`recover` for exceptions, and
strings, slices and maps.

**The target list is the decisive difference, not the runtime.** Freestanding C reaches every
architecture but only one operating system, and that is structural rather than a matter of effort:
the trick that removes the sysroot is issuing syscalls directly, and Linux is the only mainstream
system with a stable syscall ABI. macOS and Windows require their own libraries, which brings the
per-target toolchain problem straight back. A prototype Go emitter cross-built the same program to
`linux/arm64`, `darwin/arm64` and `windows/amd64` from this Linux host with nothing installed but
Go. If "cross-compile for all supported architectures on the same host, like Go" is a requirement,
the C path satisfies it only within Linux.

What it does not solve, and what a prototype has to answer before this becomes the plan:

* **Inheritance.** Go has none, so an open class still needs an emitted interface plus embedding,
  and virtual dispatch still needs designing. No saving here over C.
* **`Double.toString`.** Go prints `1e+20` where Kotlin prints `1.0E20`; a prototype shim over
  `strconv.FormatFloat` got to `1.0E+20`, so this is tractable but real work — it is not free just
  because the runtime is rich.
* **Constant overflow is a compile error in Go.** `int32(2147483647) + 1` is rejected at compile
  time, where Kotlin folds it to `-2147483648`; folded constants must be emitted so they do not
  trip this.
* **Generics.** Go's have no variance and no reification.

A gap listed here earlier — that Go's memory model has no final-field freeze — no longer applies:
the native target follows Kotlin/Native's memory model, which has no such rule to reproduce. See
*Decided: Kotlin/Native's memory model, not the JVM's* below.
* **A second full compiler in the pipeline**, and Go frames in every stack trace.

The emitter is a printer over checked common IR, and the lowering, tests and target model are
target-agnostic, so a Go emitter is a sibling of the C one rather than a rewrite. That is the
experiment to run, and this section stays open until it has been.

#### The text round-trip tax, measured — and why it cannot be skipped

Emitting a host language means printing text that the host compiler must then lex, parse and
type-check back into an IR we already had. That is real duplicated work, and it was measured rather
than estimated. Generated code in the shape an emitter produces — many small functions, explicit
width-typed arithmetic, interface dispatch — compiled on this host:

| generated size | Go: parse+typecheck share of compile | clang: frontend share of compile |
|---|---|---|
| ~6.5k lines | 24 % (0.37 s total) | ~12 % (0.39 s total) |
| ~60k lines | **39 %** (2.60 s total) | **~46 %** (3.66 s total) |

Two things follow. The tax is real and grows with generated size — roughly 40 % at 60k lines. And it
is **not a Go tax**: clang's share is the same or worse, and its total is larger. The round-trip
therefore does not discriminate between emitting Go and emitting C at all. What it discriminates
between is *emitting a host language* and *emitting machine code ourselves*, which is the
Cranelift-or-own-backend decision this document already defers.

**Using Go's runtime is easy — emitting Go source IS how you use it.** That bears stating plainly,
because "we cannot skip Go's frontend" reads like "we cannot use Go's runtime", and those are
different claims. The runtime, its collector, its scheduler and 48 target platforms are all
available; the measured entry fee is the ~5–9 % round-trip tax above. The rest of this section is
only about whether that fee can be avoided, and the answer is no.

**Three ways to reach the runtime without Go's compiler, all tested on this host, all closed.**

*Linking `-buildmode=c-archive`.* Go can be built as a C archive and linked into a C program, so the
Go runtime and its collector are genuinely present in the process. It still does not give emitted C
a garbage-collected heap: Go refuses to hand a Go pointer to C at all, at the boundary, before any
GC question arises —

```
panic: runtime error: cgo result is unpinned Go pointer or points to unpinned Go pointer
```

The sanctioned escape is `runtime.Pinner`, and pinning is the opposite of what is wanted: a pinned
object is never moved *and never collected* until explicitly unpinned. Allocating a million
short-lived objects through a pinning allocator and forcing two collections leaves **1,000,000 of
1,000,000 still live**, at 86 MB resident for 64 MB of payload. That is manual memory management
with extra steps, which is what a collector exists to remove.

*Injecting into Go's IR.* Go's compiler does have internal IRs — a typed `ir` after type-checking,
then `ssa`. Neither is reachable: both are `internal/` packages, and the restriction is enforced by
the compiler rather than by convention.

```
use of internal package cmd/compile/internal/ir not allowed
use of internal package cmd/internal/obj not allowed
```

Even with that barrier removed, the injection points do not pay. Building `ir` nodes requires
constructing correct `types2` objects, which *is* type-checking — so it would save the parse and not
the type-check, in exchange for tracking an unstable internal IR across every Go release. Injecting
lower, at `ssa`, skips the middle-end *and* skips the liveness pass that produces the stack maps,
which puts them back on us.

*Writing object files.* Same `internal/` barrier, plus an unstable format.

So **Go source is the API** to the Go runtime. The text round-trip is not an accident of how an
emitter would be written; it is the only stable, supported interface to that collector. The
question is therefore whether the fee is worth paying, which the measurements above answer: about
5–9 % of build time, for a runtime that would otherwise take years.

**Why the fee cannot be engineered away.** The reason is structural
rather than a matter of effort. Producing Go object files directly requires supplying, per function:
machine code for the target; `FUNCDATA_ArgsPointerMaps` and `FUNCDATA_LocalsPointerMaps` — the
precise GC stack maps — plus `PCDATA_StackMapIndex` saying which map is live at each PC; pcln
tables; frame size, args and locals with the `morestack` prologue protocol; write barriers at every
pointer store; the per-architecture `g` register convention; and type descriptors with GC bitmaps
and itabs. Producing correct stack maps requires liveness analysis over a register-allocated
function — which means **"skip Go's frontend" and "write the code generator" are the same project**.
There is no intermediate position where Go's collector and scheduler are reused but Go's compiler is
not. On top of that, `cmd/internal/obj`'s format is internal and unstable; it changed materially
across several releases and carries no compatibility promise.

Two cheaper levers exist and are worth taking whichever host language wins:

* **Emit one Go package per Kotlin module, not one per program.** Go then compiles packages in
  parallel and caches unchanged ones — and the ABI-based avoidance this document's phases 3–5 build
  means an unchanged module is never re-emitted, so its parse cost is not paid at all. The tax
  above is a *cold-build* number; incrementally it applies only to modules that actually changed.
* **Emit compact text.** The cost is proportional to source size, so fewer temporaries and less
  redundant spelling cut it directly.

**Measured against a real module: the tax is ~5 %, not ~40 %.** The share above is of the *host
compile*, which is the wrong denominator — what matters is the share of the whole build, krusty's
own frontend and lowering included. Measured end to end on a 24,204-line Kotlin module compiled by
the native backend, which produced 44,211 lines of C (a **1.83×** expansion) that compiles, links
and runs correctly:

| stage | time |
|---|---|
| krusty: frontend + lowering + emit (release build) | **10.1 s** |
| clang: full build and link of the emitted C | 1.70 s |
| …of which parse and sema — the duplicated work | **~0.6 s** |
| **round-trip tax as a share of total build** | **~5 %** |

At the matched generated size Go's own numbers give 2.64 s total with 1.09 s of parse, so the same
module through a Go emitter would be roughly 1.09 s of 12.7 s — **~9 %**. Both are single-digit
percentages of a real build, because krusty's frontend is the expensive part by an order of
magnitude: it runs at about **2,300 lines/s on real Kotlin** (measured on 9,151 lines of the Kotlin
standard library's own sources, an optimized build, process startup netted out), against roughly
24,000 lines/s for the whole Go compile and 16,500 for clang.

Two cautions on those figures, pulling in opposite directions and neither yet bounded. Stdlib
sources are unusually hard Kotlin — generics, `inline`, `expect`/`actual` — which raises krusty's
share and so *understates* the tax. The 1.83× expansion comes from simple top-level functions, the
only subset the native backend currently accepts; real Kotlin with classes and generics will expand
further and so *overstates* it. Treat ~5 % as the right order of magnitude rather than a precise
number, and re-measure once the backend can compile classes.

One measurement note worth keeping: the first attempt used the `gate` profile, which is
`opt-level = 0`. That put krusty at 477 lines/s instead of 2,342 — a 4.9× error, in the direction
that would have made the tax look negligible. Compiler throughput comparisons must use an optimized
build.

#### Decided: krusty owns its runtime

The measurements in this section answer "what is cheapest". They do not answer the question that
actually governs, which is what krusty is *for*. Emitting Go makes krusty a transpiler in front of
someone else's toolchain: the collector, the object model, the scheduler and the target list would
all belong to the Go project, and krusty would own none of the parts that decide how a Kotlin
program behaves at run time. **The decision is that krusty owns its runtime.** No benchmark
overrides that, and the sections below should be read as cost information for a choice already made,
not as an open argument.

One structural consequence is worth stating, because it is an argument *for* this choice that the
cost analysis misses:

**The runtime is the durable asset; the emitter is disposable.** A runtime krusty owns — object
model, allocator, collector, strings, collections, exceptions, threads — survives a change of code
generator. The C emitter can be replaced by Cranelift, or by a hand-written backend, and the runtime
carries over unchanged. Emitting Go has the opposite shape: nothing accumulates, and there is no
incremental path from it to owning anything. It is all-or-nothing in both directions.

That makes the sequencing clear, and it is not the multi-year path if taken in the right order:

1. **Keep the C emitter as the scaffold.** It exists, it cross-compiles, and its output runs. It is
   explicitly not the destination (see *The collector is where emitting C stops being viable*).
2. **Build the runtime krusty owns, in dependency order** — object model and type descriptors, then
   allocator, then a **conservative** mark-sweep collector, then strings as real objects, then
   classes and vtables, closures, exceptions, collections. A conservative collector is the enabling
   choice here: it needs no stack maps and therefore no code generator, so the runtime can be built
   and tested *while* the emitter is still C.
3. **Own the code generator when precise collection is worth it.** At that point the stack maps
   become available, the collector can move to precise and generational, and every other part of the
   runtime is already written and tested.

The cost of step 2 is real but it is not the multi-year item: the multi-year item is a *precise*
collector, and what makes it multi-year is the compiler-side stack-map, safepoint and barrier work,
not the collector's own lines. Deferring precision defers that, without deferring ownership.

**Landed:** the first three items of step 2 exist. `src/native/runtime.rs` emits the object model —
every heap object starts with a `KType` descriptor naming its reference fields, and the built-in
values (boxes, `Unit`, `String` and the byte array that holds a string's text) sit on it — and
`src/native/gc.rs` emits `krusty_gc.c`: a size-class allocator and a stop-the-world mark-sweep
collector with conservative roots and precise heap tracing, triggered by allocation volume, with
large objects unmapped when they die. `tests/native_gc_e2e.rs` drives it from C and pins each
property (garbage reclaimed, reachable objects intact, cycles collected, interior pointers rooted,
a pointer hidden in a `Long` field NOT rooted, freed slots reused); the runtime's syscall shim and
page mapping moved into a shared `krusty_sys.h`.

Classes and vtables — the fourth item of step 2 — exist now. `src/native/classes.rs` lays a class
out (header, superclass fields as a prefix, own fields aligned to their C size, the whole asserted
with `_Static_assert` in the generated C) and builds its vtable from the IR's own override edges:
classic single inheritance, every table beginning with `kotlin.Any`'s `equals`/`hashCode`/
`toString`. `KType` grew `super`, `vtable` and `vtable_length`; the object header did not, so the
collector's contract is untouched and dispatch is `obj->type->vtable[slot]`. Construction goes
through `kt_gc_allocate` and an emitted constructor that runs the superclass's first; `is`/`as`
walk the `super` chain, and a failed `as` fails loudly as the placeholder for ClassCastException;
an `object` is a lazily constructed instance in a static slot registered with
`kt_gc_add_global_root`, the first consumer of that API. `tests/native_classes_e2e.rs` runs each
of these — including a 40,000-node chain built under collection pressure, which is the test that
catches a wrong reference-offset table. `docs/SPEC.md` records every semantic decision with its
test.

What is NOT there, and still declines by name: interface dispatch (no itable — a class's vtable is
its superclass chain's, and an interface method has no fixed slot across unrelated implementors),
data classes, `inner` classes, enums, secondary constructors and constructor default arguments, a
class used from another file of the module (this needs a per-module generated header of layouts
and `extern` descriptors, which the build layer will want anyway), an override that changes a
parameter's machine representation (no bridge methods), closures, exceptions (a failed cast and a
`null` unboxing exit instead of throwing), collections, and threads. Strings still render through
the runtime rather than being a Kotlin class; there is one thread and no synchronization; and
root-finding stays conservative until step 3.

What this concedes, honestly: no macOS or Windows targets while the runtime is freestanding-Linux
(a stable syscall ABI is what makes sysroot-free cross-compilation work), no moving or generational
collection until step 3, and strings, collections and exceptions written by hand. Those are the
price of owning the thing, and they are paid deliberately.

#### The decision criterion: delete the multi-year item, keep the emitter lean

Stated plainly, the goal for the native target is to **remove the multi-year part of the
implementation while keeping a small, fast emitter**. That criterion is sharper than "C or Go", and
it settles the question, because the two halves are not independent: *where the multi-year work goes
determines how large the emitter has to be.*

The emitter is small only when Kotlin's constructs map onto constructs the target already has. Where
they do not, the cost does not vanish — it splits across the emitter *and* a runtime we then own:

| Kotlin construct | emitting C | emitting Go |
|---|---|---|
| `String` | runtime type, concatenation, rendering | native `string` |
| `class`, `open`/`override` | hand-emitted vtables | struct + interface |
| lambda / closure | environment structs + function pointers | native closure |
| `throw` / `try`-`catch` | an unwinder (freestanding C has none) | `panic` / `recover` |
| `List`, `Map`, `Set` | write the collections | slices / maps |
| allocation | write a collector — the multi-year item | precise and concurrent, free |
| threads, atomics | raw `clone()` and futexes, per architecture | goroutines, `sync` |
| targets | Linux only, by syscall ABI | 48 `GOOS`/`GOARCH` |

Every row in the C column is emitter work *plus* runtime work. Every row in the Go column is a
lowering the emitter already knows how to write. So the Go emitter is not merely a same-sized
sibling of the C one — it is **structurally smaller**, because more of Kotlin lands on something
that already exists.

The C emitter in this tree is 1,110 lines and supports top-level functions, arithmetic, control flow
and strings-through-a-runtime. Adding classes, closures, exceptions and collections to it grows both
columns; adding them to a Go emitter grows only the first, and less.

Measured against the criterion:

| approach | multi-year item removed? | emitter stays lean? |
|---|---|---|
| **emit Go** | yes — collector, threads, exceptions, collections, 48 targets | yes, and smaller than the C one |
| emit C + a conservative collector | mostly — Boehm needs no compiler support | no: vtables, closures, unwinding, collections are all ours |
| own code generator + MMTk | no — stack maps, safepoints and barriers remain | no, this *is* the multi-year item |
| port Go's runtime sources | no — and inherits a compiler-coupled codebase | no |

Only the first row satisfies both halves *on cost alone*. It is not the chosen row: emitting Go
makes krusty a transpiler rather than the owner of its runtime, which the section above settles
against. The second row — emit C, own a conservative collector — is the chosen shape, and this table
is the honest statement of what it costs: vtables, closures, unwinding and collections are ours to
write, and the target list is Linux-only until a code generator of our own arrives.

#### Settled: GraalVM is the oracle, not the pipeline

krusty already emits JVM bytecode byte-identical to kotlinc's, so `krusty → bytecode →
native-image` is a pipeline that exists today and needs no new backend at all. It was measured
rather than reasoned about. Same `fun main() { println(…) }`, this host (4 cores, 15 GB):

| pipeline | build, cold | build after a one-character edit | peak RSS | binary | startup |
|---|---|---|---|---|---|
| krusty → C → clang + lld | 0.2 s | 0.2 s | negligible | 10 KB | 0.9 ms |
| krusty → Go → `go build` | 0.2 s (warm target) | 0.2 s | ~0.3 GB | 2.2 MB | ~1 ms |
| krusty → bytecode → `native-image` | 40.3 s | **40.1 s** | **1.4 GB** | 13.6 MB | 4.4 ms |
| krusty → bytecode → JVM | 1.2 s | 1.2 s | — | — | 39 ms |

**The decisive number is the second column, not the first.** `native-image` has no incrementality:
its closed-world analysis re-runs in full for a one-character change, so the edit-compile loop costs
39 seconds every time. That is structural, not a tuning problem — the analysis is what produces the
binary. And these are floor numbers for a single class with no reflection and no framework; real
applications are minutes and many gigabytes. Against a stated goal of competing with Go on build
time and resource use, this is ~200× on time and ~5× on memory, in the wrong direction.

So native-image is not the build path. It is, however, the **native correctness oracle**, and a good
one — which the native track otherwise lacks entirely, since Kotlin/Native's output cannot be
compared byte-wise against anything. The same source compiled by krusty and run under a
native-image binary has exact JVM semantics: the JVM memory model, a real collector, and the whole
stdlib. Comparing our fast native output against it differentially mirrors what kotlinc already
does for the JVM backend, and it costs 40 seconds in CI rather than 40 seconds per keystroke.

#### The collector is where emitting C stops being viable

A precise collector has to enumerate the live references on the stack at a safepoint, which needs
stack maps, which needs control over frame layout. Emitting C gives that control to the C compiler,
so the stack maps cannot be produced. That leaves exactly two options, and both are bad:

* **Conservative scanning** (Boehm-style): read every stack word and treat anything that looks like
  a heap pointer as one. It works, and it forecloses moving collection — so no compaction, no
  bump-allocating nursery, no generational copying — and retains garbage whenever an integer
  happens to look like an address.
* **A shadow stack**: emit an explicit push and pop of every live reference into a side stack.
  Precise, at the cost of a store and a load per reference per call, everywhere.

This matters because "precise versus conservative GC" is listed below as one of the three decisions
that shape the low native IR. Emitting C does not leave it open — it answers it *conservative*, by
construction, without anyone deciding. That is the clearest argument that the C backend is a
scaffold with an expiry date rather than a target to grow features on. Go's collector is precise and
concurrent because the Go compiler emits the stack maps; SubstrateVM's is precise too, at 39 seconds
a build.

#### What a runtime of our own would cost

"Write our own Go-like runtime" is the alternative to emitting a host language, so it is worth
sizing rather than waving at. Go's runtime, measured on this host, is **105,680 lines of Go plus
61,190 of assembly** across eleven architectures.

Most of that is not ours to write. Under Kotlin/Native's memory model the target uses OS threads,
and `suspend` is already CPS-transformed before the backend, so green threads, growable stacks,
channels and Go's own map implementation — 18,318 lines together — are all out of scope. What
remains is:

| component | Go's line count |
|---|---|
| garbage collector | 7,010 |
| allocator | 6,280 |
| OS layer (one OS) | 2,392 |
| panic / unwinding | 1,532 |
| type descriptors + interface dispatch | 1,365 |
| sync primitives | 874 |
| **total, one OS** | **~19,500** |

plus a few hundred lines of assembly per architecture for context save, atomics and thread entry.

Against krusty's ~496,000 lines that looks like 4 % of the project, and that comparison is the trap.
Runtime code is not compiler code: it is concurrent, it is memory-ordering-sensitive, its bugs are
non-deterministic, and — unlike every other part of krusty — **it has no oracle**. There is no
`kotlinc` to diff a collector against.

**The runtime is also not the expensive half.** Each of these lives in the *code generator*, not in
the line counts above:

1. **Stack maps** — at every safepoint, which references are live in which slots and registers.
   Requires liveness analysis over a register-allocated function. This is the same wall recorded
   above under *The collector is where emitting C stops being viable*.
2. **Write-barrier insertion** at every pointer store, with elision rules — a barrier on every store
   without them is a large, pervasive slowdown.
3. **Safepoint placement** at call sites and loop back-edges, so threads can be stopped.
4. **Per-type GC bitmaps**, emitted by the compiler.

So "write a Go-like runtime" is really "write a code generator with GC support, and then a runtime",
and the first half is the larger one. Three honest tiers:

* **Stop-the-world precise mark-sweep, OS threads.** Perhaps 4–8k lines of runtime — but it still
  needs the whole stack-map and safepoint apparatus, so the compiler work dominates. This is the
  floor, and it is already a major project.
* **Parity with Kotlin/Native** — concurrent mark-and-sweep, non-generational, write barriers.
  Roughly 15–25k lines plus the codegen work. JetBrains rewrote Kotlin/Native's memory model once
  already (the freezing model, replaced in 1.7.20); that was a multi-year effort with a team.
* **Go-class** — sub-millisecond pauses, hybrid write barriers, escape analysis feeding stack
  allocation. ~100k lines and a team indefinitely. Go's collector has been rewritten more than once.

For calibration at the other end: when this estimate was written the C runtime this repository
shipped was **617 lines** and had no collector, no threads and no exceptions. The distance from
there to the first tier is the quantity in question.

**Can we take Go's runtime sources and build our own library from them?** Legally yes — Go is
BSD-3-Clause, so vendoring or deriving is permitted with attribution. Technically it is not a
library. It is a program written in a private dialect of Go that only Go's own compiler implements:
**362 of 445** non-test runtime files carry `//go:` pragmas, among them 986 `//go:nosplit` (omit the
stack-growth check), 766 `//go:linkname` (alias a symbol across packages), 283 `//go:nowritebarrier`
(assert to the compiler that no barrier is needed here), 211 `systemstack(` calls (switch to the
scheduler's stack) and 484 `getg()` (read the current goroutine from a reserved register). Every one
of those is an instruction *to the Go compiler*. "Build a library from these sources" begins with
implementing that pragma set in whatever is going to compile them.

What separates cleanly and what does not:

* **The allocator** (~6,280 lines — size classes, spans, per-P caches, tcmalloc-derived) is the
  portable part. Porting it is transliteration against our own thread model, not copying.
* **The collector does not port.** Its write barriers are *compiler-inserted* — the 237
  `gcWriteBarrier` sites are generated, not written; its stack scanning consumes Go's own stack-map
  format; and its pacer is welded to the P/M scheduler.

And it leaves the expensive half untouched: stack maps, barrier insertion and safepoints are
compiler-side no matter whose collector runs.

**The precedent is discouraging.** TinyGo exists to compile Go, has every incentive to reuse Go's
runtime, and wrote its own anyway — a conservative mark/sweep collector and LLVM coroutines in place
of Go's scheduler, because Go's runtime assumes preemptive multi-threaded scheduling and stack
switching it could not provide.

**The better version of the idea is to take a collector built to be embedded, not one welded to a
compiler.** [MMTk](https://www.mmtk.io/) is a Rust crate of production collectors (Immix,
mark-sweep, generational copying) designed for exactly this: it makes no assumption about the host
runtime's implementation language, and has bindings for OpenJDK, V8, Julia and Ruby. It is dual
MIT/Apache-2.0. That it is *Rust* matters here more than anywhere — krusty is Rust, so it links
directly rather than through a foreign-function boundary. It does not remove the compiler-side work
— we would still supply roots and stack maps — but it removes writing a collector, which is the
part of the runtime estimate above with no oracle and the worst failure modes.

The caveat is a project-values one, not a technical one: MMTk is a substantial dependency, and
`CLAUDE.md` keeps this tree deliberately dependency-lean. That is the same explicit decision the
code generator poses, and it should be made the same way — deliberately, not by drifting into it.

**The one cheap option, and its price.** A conservative Boehm-style collector needs no compiler
support at all — no stack maps, no safepoints — and would work with emitted C. Perhaps 1–3k lines,
or a vendored dependency. It buys a working GC today and permanently forecloses moving collection:
no compaction, no bump-allocating nursery, and retained garbage whenever a stack word happens to
look like a pointer. If the C path is ever to ship, this is how it gets a collector; it should be
chosen deliberately rather than arrived at.

Set against all of this: emitting Go supplies the third tier for zero lines and zero maintenance.
That is the substance of the Go-versus-own-backend decision, and it is much larger than the ~5 % 
round-trip tax measured above.

#### Decided: krusty owns the code generator too — no C emission

*Decided: krusty owns its runtime* drew the ownership line at the runtime and kept the C emitter as
a scaffold, on the argument that the runtime is the durable asset and the emitter is disposable.
The user has moved the line: **krusty emits no C.** Compiling a user's program must not involve
another language's compiler at all, for the same reason emitting Go was rejected — a compiler that
prints source for another toolchain to compile is a transpiler, whatever it owns underneath. The
requirement is Go's: krusty's own code generator, **fast and incremental**, from one host to every
target.

What that changes, and what it does not:

* **The runtime survives unchanged.** Allocator, collector, object model, `KType` and vtables,
  strings, `_start`, syscalls — ~1,470 lines of freestanding C — are krusty's, and nothing about
  them depends on how user code is generated. They are compiled **once, when krusty itself is
  built**, for every supported target, and shipped inside the compiler as prebuilt objects. That is
  exactly Go's arrangement: Go's runtime is compiled by the Go toolchain into archives that ship
  with the distribution. A *user's* build then needs krusty and nothing else. The one honest
  consequence: building krusty needs a C cross-compiler (`clang` targets every architecture from
  one host), which is a build-time dependency of the compiler, not of anyone using it.
* **The C emitter is retired — done.** `src/native/emit.rs`, `backend.rs` and `link.rs` (about
  2,300 lines) lowered common IR to C text and drove a C compiler; they are deleted. The class
  slices landed against them were not wasted: their tests were Kotlin programs with expected
  output, and every one of them now runs through the code generator. The class model half of
  `classes.rs`, the runtime and the object model stayed.
* **A linker becomes krusty's.** Zero-toolchain cross-compilation — the property this whole track
  is built on — needs the final link done by krusty, not by a system `ld`: Go has its own linker
  for exactly this reason. Scope for a static executable from a handful of objects is bounded
  (symbol resolution, the relocation kinds three architectures use, program headers); it is not
  Go's linker, which does far more.
* **Incremental means per-module objects, cached by ABI hash.** That is the build layer this
  document began with, applied to native: a module compiles to an object once, dependents rebuild
  only when its ABI moves, and the link is the only whole-program step — as in Go.

**Decided: Cranelift, as a library.** The last open question was the project-values one the *Risks*
section reserved, and it is closed: krusty drives **Cranelift** (a Rust library; measured at 46 transitive
crates against a compiler library that has four today; Go-league compile speed; x86-64, AArch64,
RISC-V and s390x backends already written), or **hand-write** the instruction selection and
encoding for each architecture (maximal ownership, as Go did; the truly multi-year item across three
targets). The lowering from common IR, the ABI, object layout, GC integration and the linker are
krusty's under either choice; what Cranelift owns is instruction selection and register allocation,
and only that. The dependency is taken knowingly: it is the first break from this tree's four-crate
compiler library, made because the alternative is the multi-year item this track exists to avoid.
The lowering stays behind a narrow seam so a hand-written backend could replace Cranelift one
architecture at a time if that ever becomes worth doing.

**Sequencing, in runnable increments — every commit runs a Kotlin program that could not run
before:**

1. **Landed.** `fun main() { println("Hello, world!") }` runs through krusty's own code generator
   on linux-x86_64 (`tests/native_codegen_e2e.rs`): `src/native/codegen/` lowers checked IR to
   Cranelift and emits a relocatable object; `src/native/linker/` resolves symbols, lays out two
   segments, applies the five x86-64 relocation kinds and writes a static `ET_EXEC` ELF by hand;
   `build.rs` compiles the runtime once per target with clang and `src/native/prebuilt.rs` carries
   the objects inside the compiler. The executable runs with an empty environment. No C is emitted.
2. **Landed.** The same hello world links for linux-aarch64 and linux-riscv64 from the same host
   (`one_host_links_a_static_executable_for_every_supported_architecture`): Cranelift's AArch64 and
   RISC-V backends are compiled in, and the linker applies the nine AArch64 and eleven RISC-V
   relocation kinds their objects and the prebuilt runtime use — RISC-V's `PCREL_LO12` paired with
   its `HI20` in a second pass, `RELAX` ignored because this linker does not relax. Verification is
   honest about its limit: the host binary is run; the cross-built ones cannot be executed here
   (no emulator), so every call and literal-pool relocation in their program text was decoded from
   the linked image and compared against symbol addresses computed independently from the input
   objects' own tables — all match. Running them is one `qemu-user-static` install away on CI.
3. **Landed.** Every test written against the C path runs through the new generator, and the C
   path is deleted. First the hello-world suite — arithmetic with Kotlin's wrapping, division and
   shift rules, `Byte`/`Short`/`Char` widening, locals, `if`/`when` as statement and as value,
   `while`/`do…while`/lowered `for` with labeled `break`/`continue`, early `return`, recursion,
   `compareTo`, string templates, `String.plus`, boxing for `Any?` positions, `== null` and `===`,
   the 200,000-iteration allocation loop under collection. Then the class suite
   (`tests/native_classes_e2e.rs`, rewritten in place): `KType` descriptors emitted byte for byte as
   `krusty_rt.h` declares them, vtables as tables of function addresses, constructors that run the
   superclass's first, field loads and stores at the model's offsets, dispatch through the
   receiver's descriptor with a checked null receiver, `super` as a direct call, synthesized
   accessors for open properties, `is`/`as`/`as?` through the runtime, `object` singletons in a
   registered root slot, and the 40,000-node chain built under collection. Finally the collector's
   C-program test (`tests/native_gc_e2e.rs`) compiles its program with clang and links it with
   krusty's linker against the prebuilt runtime, so `emit.rs`, `backend.rs` and `link.rs` are gone
   and nothing in the tree drives a C compiler at a user's build. The cross-architecture link
   carries a class hierarchy for every target.
4. Per-module objects and ABI-hash caching — the incremental half.
5. **Landed (first cut).** The `codegen/box` corpus runs through the native pipeline
   (`tests/kotlin_box_native_conformance.rs`, the native lane of the `conformance` binary, run by
   CI next to the JVM lane): each single-file case compiles with a program entry that prints
   `box()`, links against the prebuilt runtime, and runs; skipping is permitted, miscompiling
   never. First full run against Kotlin 2.4.10's 7,352 cases, in 63 s on one machine: **359
   pass**, 2,974 declined by construct, 1,828 rejected by the frontend, 2,182 outside the lane
   (multi-file, multi-module, other backends, unmodeled flags), 2 frontend panics, 7 known
   failures shared with the JVM lane and listed with their reasons in
   `tests/native_box_expected_failures.txt` (a listed case that starts passing fails the test until
   it is removed), **0 unexpected failures**. The lane honours the corpus's JVM ignores as well as
   its native ones: krusty has one frontend and one common lowering, so a case muted on `JVM_IR` is
   muted for a reason the native backend inherits rather than causes — verified case by case by
   running the same source through the JVM backend and getting the same wrong answer. The first run found and fixed two native defects —
   `===` on primitives boxed both sides and compared addresses; `UInt` was carried as the `Int` it
   wraps — which is what the gate is for. The backlog, by frequency: top-level properties (499),
   structural `==` on references (152), lambdas and function values (264), checked operations
   (70), reference arrays (50) and array intrinsics (85), callable references (49), data classes,
   interfaces, inner and local classes, value classes, `vararg`, `!!`, extension properties,
   floating-point rendering, `try`.
6. **Growing the generator by that backlog, biggest first.** Top-level properties (499 declines),
   structural equality on references (171), `x!!` (33) and function values — lambdas, invocation
   and captured-variable holders, 310 between them — have landed, each leaving the decline table
   entirely: **778 pass**, more than double where the lane started, with 0 unexpected failures
   throughout. The collector's own suites — invariants from C, stress through the generator — are
   what caught the one real miscompile of the session: a `var` a closure captures is replaced by a
   holder, and the generator believed the `Int` the declaration still said, truncating a pointer
   into a 32-bit slot. The corpus passed 7,352 cases with that bug, because no case keeps something
   alive across a collection and reads it back. Value classes were tried and put back: common IR carries one as an ordinary class,
   which compiles and then answers `IC(1) == IC(1)` with identity — 27 wrong answers the lane
   caught at once. They stay declined until the generator realizes the equality, hashing and
   rendering Kotlin gives them. Realizing them exposed three native defects the gate
   caught and that are now fixed with tests — a `when` whose arms disagree on a carrier was typed
   from its first arm, boxed small values were not cached so `===` on them was false, and a
   companion object's initializers never ran; and a capture-free lambda was a fresh object per
   evaluation where Kotlin makes it a singleton — plus five more corpus cases traced to common
   lowering (its `++`/`--` shape, a context parameter's receiver, an extension call's evaluation
   order) by getting the same wrong answer from krusty's JVM backend.
   Arrays, `Unit` as a value and callable references followed, taking the lane to **813 pass**.
   The last of those slices ended with a lesson about the gate itself: three corpus cases passed in
   CI and failed in this container, because both were compiling a `tailrec` the checked lowering
   does not turn into a loop and whether a million frames fit is the machine's business, not the
   compiler's. A gate must not depend on that. So `IrFile::unlooped_tailrec` now records every
   `tailrec` left recursive — a member, an extension, a context-parameter or a local one — and the
   generator declines it rather than emitting a program that dies on a guard page. The same
   investigation closed a real gap: a `Unit` call whose only successor is `return` IS a tail call in
   Kotlin, and the checked lowering now rewrites it, which makes `unitBlocks.kt` pass outright. The
   expected-failures list dropped from 14 entries to 8, and every one that remains is a wrong ANSWER
   krusty's JVM backend gives too, not a crash that depends on where it runs.
   The next slice cost two lines and bought **937 pass**. `x.apply { … }`, `let`, `run` and `also`
   were the largest single decline in the table, and the reason was not that the generator could not
   compile them: the checked lowering had ALREADY spliced each one into its caller, exactly as
   `inline` means, and then cleared the block's standalone implementation because nothing calls it.
   The native backend was reading that cleared implementation as a function it could not compile,
   and the orphaned lambda node left in the expression arena as a function value to build a thunk
   for — a thunk calling a symbol nobody defines, which fails the LINK rather than the compile. Both
   now skip what the splice made dead. The lesson generalizes past this slice: a decline is a claim
   about what the generator cannot do, and it is worth checking that the claim is true before
   building the feature it asks for.
   What survived that check was the other half of the same entry, and it took the lane past a
   thousand: **1002 pass**. A scope function whose block arrives as a function-typed PARAMETER has
   no body to splice — `fun build(instructions: Buildee<T>.() -> Unit) = Buildee<T>().apply(
   instructions)`, the shape every `inference/pcla` case is built on — so the call reaches the
   generator and is realized as what it means: invoke the block on the receiver, yield the receiver
   or the block's result as Kotlin's signature says.
   Extension and context properties followed for **1031 pass**: one has no storage, so each access
   is a call to the accessor the checked lowering already built, in the parameter order that
   lowering recorded. A member extension property still declines — it can be overridden, so it
   wants its receiver's vtable slot rather than a direct call.
   Data classes took the lane to **1082 pass**, and they were another decline worth checking before
   building: their `equals`, `hashCode`, `toString` and `componentN` are already synthesized by
   common lowering, so all the generator owed them was the per-field hash and comparison those
   members are written in terms of. The corpus then caught what the synthesis itself had wrong — a
   data class with a NULLABLE array field rendered the array by identity, because `is_array` is
   false for `Array<Int>?` — and krusty's JVM backend printed the identical wrong answer, which is
   what says the defect is common lowering's. Fixing it there exposed a second one underneath, in
   the JVM realization: `java.util.Arrays.toString` has no `Integer[]` overload, so a reference
   array has to name the `Object[]` one. Both are fixed, and both lanes now pass the case.
   Interfaces were the largest remaining construct and took the lane to **1234 pass**. A call
   through an interface-typed value knows only the interface, so the slot it dispatches on has to
   mean the same member in every class implementing it: each class's table is its own slots, padded
   to a common base, then one entry per interface member in the program. Dispatch stays a single
   indexed load — the same instruction a class method's call site emits — at the cost of a vtable as
   long as the program's interface surface. That is the right trade while a compilation unit is a
   file; an itable search is what to revisit when it is not. `is` needed the other half: an
   interface is not on the single-inheritance chain, so each descriptor carries the interfaces it
   implements, flattened. The corpus's `bridges/` directory then did its job twice over — a fake
   override, where a class satisfies an interface with a method it inherits from a superclass that
   knows nothing of the interface, and the case where that inherited method returns an unboxed
   `Int` for an interface promising `Any`, which needs a bridge and is declined rather than
   miscompiled. An interface member a concrete class implements but this model cannot find is a
   decline too, not an abstract trap: the implementation is in the source, so failing to find it is
   this model's defect and belongs at compile time.
   Value classes came back next, for **1460 pass**, and this time they stayed. What made them a
   miscompile before was never the class — it was the three members: Kotlin answers `equals`,
   `hashCode` and `toString` by the value inside, and an ordinary class answers all three by
   identity. Synthesizing them beside the object is about a hundred lines and needs none of the
   JVM's erasure, mangling or box adapters, because the object here is already ours and the
   collector already traces it. The corpus then caught a defect that had nothing to do with value
   classes and had been sitting behind their decline: `T : Int` is carried as a boxed `Int`, and
   the operator path unified operands by MACHINE type, so a pointer and an integer "unified" into
   pointer arithmetic that printed as an answer. Operators now unbox through the bound, and the
   result of one is the primitive rather than the reference the declaration spells.
   The suite itself then became the second corpus. Every test that asserts `box()` prints `OK` now
   compiles, links and runs its source through the native backend as well — 599 programs written to
   pin krusty's own semantics, with the oracle the native lane already uses, for about 6% of the
   suite's wall time and no second suite to keep in step. A decline is a skip; an accepted program
   that prints anything but `OK` fails the test it came from. The first run found a defect the
   7,352-case corpus never had: a Kotlin `fun cast(…)` is named `kt_cast`, which the runtime also
   defines, and the link failed with a duplicate symbol that said nothing about the Kotlin name
   behind it. The generator now reserves the symbols the prebuilt runtime defines, read out of the
   runtime objects rather than listed beside them.
   SAM conversions followed for **1508 pass**, and they were cheap only because interfaces had
   landed first: a lambda converted to a `fun interface` is the same object holding the same
   captures, wearing that interface's table instead of the single invoke slot. The corpus insisted
   on two details — the table starts from the interface's OWN, so a default method answers, and a
   `fun interface` that overrides `toString` keeps `kotlin.Any`'s slot 2, which is where the
   runtime's rendering looks — and on one piece of Kotlin semantics: converting a nullable function
   value yields null, not a wrapper around nothing.
   `inner` classes took the lane to **1574 pass** and were two declines standing in front of one
   small realization: the outer instance is a field, written before the superclass constructor
   because that is Kotlin's order, and `this@Outer` is a load of it. The corpus then produced two
   cases where an `inner` class also EXTENDS its outer, and krusty's JVM backend gets both wrong in
   its own way — one answers `this@Outer` with `this`, the other emits bytecode the verifier
   rejects — so both are listed with that evidence rather than chased in the generator.
   Defaulted calls took the lane to **1644 pass**. A default belongs to the callee's frame — it may
   read an earlier parameter — so each omission shape gets a wrapper that declares that frame, fills
   the missing slots in declaration order and calls through; one wrapper per shape a program
   actually uses, and no mask to decode at run time. Data-class `copy` came with it. Writing it
   surfaced a live miscompile in code already pushed: a member extension's override is recorded in
   no override table, so it took a slot of its own and a call through the base's type ran the base's
   body. krusty's JVM backend gets that right, so it was the native model's alone; the IR's own
   record of which declarations are FRESH is what makes resolving it by name sound.
   Secondary constructors brought the lane to **1666 pass**: each is its own entry point that
   delegates and then runs its body, including for a class with no primary constructor at all,
   which then declares no primary `<init>` rather than declaring one nothing defines.
   Enums followed, for **1735 pass**. An enum reaches a backend almost bare — the checked IR keeps
   the constants as names and leaves every realization open — so all of it is built here: a slot per
   constant, one initializer that fills them in declaration order and then asks for the companion,
   `values()` as a fresh array each call, `valueOf` as a chain of name comparisons, and `name`,
   `ordinal` and `toString` read from the storage `kotlin.Enum` contributes ahead of the class's own
   fields. The corpus was exact about the order and worth listening to twice: the companion comes
   after every constant, and touching only a companion member still builds the constants first.
   Then one line came out and the lane went to **1767 pass**. "An adapted callable reference" had
   been declined on the assumption that an adaptation — a defaulted argument, a `vararg` given one
   element, a result discarded for a `Unit` expectation — was work the generator owed. It is not:
   the checked lowering builds an adapter function for each, and an adapter is an ordinary
   function. That is three declines this session that were guesses about work someone else had
   already done, which is why probing one before building for it has become the first step.
   Local and anonymous classes made four: the decline came out, and local classes — plain, capturing
   an enclosing local, and generic — ran unchanged, because the checked lowering lifts them to the
   file with their captures as leading constructor parameters. Only object expressions needed
   anything, and not what the decline said: an anonymous object's constructor is not its class's
   first declaration, so the lowering cannot recognize it as the primary one and names it by its
   parameter list the way it names a secondary. Construction now falls back to the primary when the
   list is the primary's own.
   Two defects came with them, and neither was the generator's kind. The first was: a mutable local
   a local or anonymous class captures is SHARED, and the field carrying it holds the cell the
   enclosing function allocated, not a copy of the value. Common IR marks that coordinate and keeps
   the element's type so no backend's holder leaks into the frontend; `native::captures` makes the
   choice for this one (a plain reference), and until it did, `var a = 1; object { init { a = 2 } }`
   left `a` at 1.
   The second was the frontend's, and both backends had it: a local class's body properties are
   numbered from the body, while the legacy source coordinate numbers the constructor's `val`
   parameters first. Reading one numbering as the other bound every property reference to the NEXT
   declaration — in `class P(val n: Int) { val a = n * 2; val b = a + 1 }`, `a` named `b` — so `b`
   read an unwritten field and `p.a` answered with `b`. It only bites when the two counts can
   collide, which is why it survived until local classes started running: with no constructor `val`
   the numbers agree, and with two the miscount runs off the end and falls back to the right answer.
   Resolving the declaration by its own SOURCE RANGE cannot drift between the conventions. The lane
   stands at **1968 pass**, with eight further corpus cases listed as known failures — anonymous
   objects passing captures to a superclass constructor, an `inner` class of a local class reaching
   two enclosing instances — each one krusty's JVM backend rejects or answers identically.
   Three more declines were read off the backlog and probed rather than built for, and all three
   were saying something about the JVM rather than about this generator, which took the lane to
   **2003 pass**. A static method OWNED by a class is a JVM placement fact: there is no facade here
   for it to be placed differently from, so it is a function with a symbol and a direct call (a
   local function declared inside a member or an `init` block is what arrives that way). A
   companion's `const val` is stored on the outer class there for the same kind of reason; here the
   owner says nothing, and the one thing it could have said something about — when the initializer
   runs — a `const` settles, because a compile-time constant cannot tell program start from the
   companion's own initialization. A non-`const` property owned by a class still declines, since
   its order IS observable. And a secondary constructor's `super()` reaching `kotlin.Any` needs
   nothing emitted: the root declares no state and no constructor, which is already why a class
   whose only supertype is `Any` calls no parent constructor.
   Three more, for **2031 pass**. `String.length` is a count of UTF-16 code units and a krusty
   string holds UTF-8, so the runtime walks the bytes: a byte that is not a continuation byte
   starts one code point, and of those only the four-byte ones are a surrogate pair and count two.
   `Any()` allocates an object with the runtime's own `kotlin.Any` type and nothing after the
   header, because the root has no state. And a class-body declaration that stores the value a
   fresh object's storage already holds — `var x = 0` — emits nothing, which is Kotlin's rule and
   not an optimization: a base constructor that dispatches to an override writes those fields
   before the subclass's initializers would, and leaving the store out is what lets the write
   survive. That rule now lives with the IR (`IrFile::is_elided_initializer_store`) rather than in
   the JVM backend, because every target krusty emits for clears an object's storage when it
   allocates and so there is one rule, not one per backend.
   Constructor defaults took the lane to **2080 pass**. They are a function's defaults with the
   object already in hand, and get the same answer: a wrapper per omission shape that declares the
   constructor's whole frame, fills the missing slots in declaration order and calls the
   constructor, with the allocation left at the call site because allocating is not a default's to
   do. `class B : A()` reaches the same wrapper — a superclass delegation that leaves arguments out
   is that call written without parentheses of its own — and needed one thing the corpus was clear
   about: common IR fills an omitted super-constructor operand with a ZERO PLACEHOLDER, so that a
   target's own default ABI has something to put a mask against, and records the ordinals it
   actually omitted beside the class. Passing the placeholders through is how `object : A() {}`
   answered `null` where `A`'s default said `"OK"`; dropping them at the coordinate the IR names is
   the fix. One more corpus case is listed with evidence: a `var` initialized from a subclass keeps
   that smart cast across the loop's own reassignment, and reduced to one file krusty's JVM backend
   throws the identical `ClassCastException`.
   Enum constants with a body followed, for **2107 pass**. `ADD { … }` is not the enum: it is an
   instance of a synthesized subclass, which is how it overrides a member and how it can declare
   state. Common IR names that subclass on the entry and records only the USER parameter types on
   it, because the JVM's enum ABI gives its constructor a leading `(String name, int ordinal)` that
   is a realization rather than a Kotlin fact. This generator stores the name and ordinal itself,
   so the subclass's constructor takes exactly those user parameters and passes them on, and
   everything else about a constant — `values()`, `valueOf`, `toString`, `is` — is unchanged
   because the subclass inherits the enum's whole layout and table. It found one gap in passing:
   `name` and `ordinal` belong to `kotlin.Enum`, which no file declares, so the checked property
   table had nothing to say about their types and a concatenation of one could not be typed.
   Then floating point, for **2152 pass** — the largest single decline on the list, and the one with
   an actual algorithm behind it. Kotlin's `toString` for a `Double` is the SHORTEST decimal that
   reads back as exactly that value, printed plainly inside `[10^-3, 10^7)` and as `d.dddEn`
   outside. That is not a formatting preference a runtime may pick: a program prints these, so
   `0.1` must not come out `0.1000000000000000055511151231257827` and `1.0E7` must not come out
   `10000000.0`. The runtime works it out with Steele & White's generator in Burger & Dybvig's
   formulation — the value and both boundaries of its rounding interval carried as exact rationals,
   scaled, then digits emitted until what is written already reads back — which means big integers
   (the intermediates reach about 2^1140) and no floating-point arithmetic anywhere, since a
   shortest-digits routine that rounded would be deciding the answer with the imprecision it exists
   to describe. It lives in `src/native/runtime/krusty_fp.c` and was verified against the JVM's own
   `Double.toString`/`Float.toString` over a million values — random bit patterns, every small
   subnormal, powers of ten and their neighbours, short decimal literals — with no disagreement.
   Two details are Kotlin's rather than the algorithm's and are written down where they are made:
   where ONE digit would do, the two-digit decimals are considered alongside it and the closer wins
   (which is why `Double.MIN_VALUE` is `4.9E-324` and not the shorter, equally round-tripping
   `5E-324`), and `equals` on a boxed value compares BITS where `==` on two `Double`s compares
   numbers — so a boxed `NaN` equals itself and a boxed `0.0` does not equal `-0.0`, each the
   opposite of the unboxed answer. `%` came with it, for **2158 pass**: Kotlin's is IEEE's
   remainder truncated toward zero, no instruction provides it on every target, and it is computed
   on the significands with a shift-and-subtract loop — exactly, so `1.0E16 % 3.0` and a pair of
   subnormals come out right where anything reaching for division would not.
   Two more questions Kotlin asks of a value directly took the lane to **2177 pass**: `is Double`,
   which needed only the runtime's own type now that a floating-point value has a box, and `isNaN`
   with its two siblings, which are one comparison each and are emitted here rather than called
   into the runtime so the operand is never boxed to ask about its bits. Between them they made a
   third thing reachable and wrong: `val c: Any = 'A' + 1` boxed an `Int` holding 66. Arithmetic on
   the narrow integer types IS `Int` arithmetic — Kotlin has no `Byte.plus(Byte): Byte` — but
   `Char` is the exception that makes the others a rule, since `Char.plus(Int)` and
   `Char.minus(Int)` are declared to return `Char` and only `Char.minus(Char)` returns `Int`. The
   result is now narrowed back to `Char`, the same `i2c` kotlinc emits after its `iadd`.

   **Interface delegation — `class C(d: I) : I by d` — took the lane to 2214 pass.** The forwarder
   the compiler synthesizes for each delegated member calls the delegate through the STATIC type
   `I`, and that is the one call shape that reaches the generator as `Callee::Virtual`: the
   receiver's own class is unknown at the call, so the slot has to be the interface's number rather
   than any implementation's. The number was already there — `place_interface_slots` gives every
   interface member one number that means the same thing in every implementation — so the call is
   the slot looked up on the DECLARING classifier and the vtable indexed on the receiver, which is
   a single indexed load like every other dispatch here. The 33 cases it was declining are almost
   all `delegation/` and `classDelegation/`, and lowering them exposed a second, older defect in
   the numbering itself: one vtable entry can be named by SEVERAL keys — `interface Base2 : Base`
   redeclaring `test` registers both its own spelling and `Base`'s at the same slot — and numbering
   them one at a time gave the second spelling a number of its own. A class that registered its
   implementation under the first then looked like it supplied nothing and silently took the
   interface's default: `hiddenSuperOverrideIn1.0.kt` answered `base 2fail` where the delegate
   answers `OK`. Members are now numbered by slot, all spellings of one entry together, which also
   makes the numbering independent of the map's iteration order — sorting by slot alone did not,
   because two keys sharing a slot compared equal.

   **Ranges as values took it to 2243 pass.** A range a loop consumes never becomes an object —
   common lowering turns `for (i in 1..10)` into a counted loop before this backend sees it, the
   same trade the JVM's own range intrinsics make. What was declining is the other half: a range the
   program KEEPS, passes or asks a question of. That one is an ordinary heap object, and the three
   closed integral ranges are the runtime's own types rather than a library's, which is the whole
   point of owning the runtime — `equals`, `hashCode` and `toString` are written where the object
   is, so they answer what the Kotlin declaration each stands for answers, and `docs/SPEC.md`
   records each decision with its test. One struct serves all three, with the bounds kept at 64 bits
   and the descriptor telling them apart; a `Char` bound is stored zero-extended so a code point
   above `0x7FFF` is not read as a negative number. `for (x in r)` over a materialized range then
   needed an iterator, which is one more runtime object carrying a "there is another" bit rather
   than a `next <= last` test — `for (i in (Int.MAX_VALUE - 2)..Int.MAX_VALUE)` is why, since
   incrementing past the maximum wraps and that test would never stop. Kotlin's own
   `IntProgressionIterator` carries the same bit. `step`, `downTo` and `reversed` still decline, and
   that is what makes reading `first`/`last` off a receiver typed as a PROGRESSION sound: with no
   way to build one, the only progression a lowered program holds is a range.

   **`x.indices` and `s[i]` followed, for 2255 pass.** `indices` is `0..size - 1` of the receiver,
   so once ranges were objects it was one subtraction and a construction — for a receiver whose
   size this generator can read, which is an array or a `String`; the same extension property covers
   `Collection`, and that one declines by RECEIVER rather than by name, because there is no
   collection runtime to answer its `size` yet. Indexing a string came with it, and it is where the
   two encodings the runtime bridges disagree most: the text is stored as UTF-8 and Kotlin indexes
   by UTF-16 unit, so `s[i]` walks the bytes the way `length` already counts them, and a character
   outside the BMP is one UTF-8 sequence and TWO Kotlin indices — read back as the surrogate pair
   Kotlin stores. Walking per access is what a string that stores UTF-8 costs; a program that wants
   to iterate cheaply iterates the string rather than its indices. Carrying the index needed one
   more thing: the dependency-member path crosses every argument as a REFERENCE, which is right for
   a member that asks about an object and boxes the very number `s[i]` is about, so a small table
   (`intrinsics::scalar_member`) names the members whose arguments cross as values.

   **A property reached through a receiver its owner does not supply took the lane to 2282.** A
   member extension property (`class C { val Foo.bar get() = … }`) and a member property with
   context parameters have no storage to reach — an extension property cannot have a backing field,
   because there is no object of its own to keep one in — so every access is a call to the accessor
   the checked lowering already built, with the operands in the order that lowering recorded. What
   makes them different from the top-level form already handled is that the accessor is an INSTANCE
   METHOD of the owner and can be overridden, so the call goes through the owner's vtable slot
   rather than straight to a body.
   Pointing that slot at the override needed a second look at the numbering. A property ACCESSOR is
   recorded in `fresh_method_decls` whether or not its property overrides one — common lowering
   pushes every accessor there without reading the modifier — so the table that tells a method from
   an override cannot be believed for one, and `override val Foo.bar` was taking a slot of its own
   while every call kept reaching the base's. The model now also matches an OPEN, non-private base
   member of the same name and machine signature, which Kotlin rejects a fresh redeclaration of
   ("hides member of supertype and needs `override`"): reading the language's rule rather than
   guessing. For an ordinary method the table is right and this never fires.

   **Property references are objects, and that took the lane to 2303.** `::foo`, `C::p` and `x::p`
   are values, and a value here is an object with an emitted type — the same shape a lambda takes,
   with more surface: `KProperty` declares `get`, `KMutableProperty` adds `set`, `KCallable`
   declares `name`, so the type carries three slots beyond `kotlin.Any`'s and the site's own bodies
   fill them. Those bodies are emitted rather than lowered, because there is no IR to lower: common
   lowering leaves the reference CHECKED — it names the property and says whether a receiver is
   bound, and nothing more — precisely so each target may choose its representation. Emitting them
   needed the property read and write split into halves taking an already-evaluated receiver, which
   is what a synthesized body has and an IR-driven one does not.
   One type per PROPERTY rather than per site is what makes the equality right. Kotlin compares a
   callable reference by the declaration it names, so `::foo == ::foo` is true although each is
   written in its own place; with one type per property the type IS the declaration, `equals` is a
   pointer comparison plus the bound receivers when there are any, and an unbound reference has one
   instance for the whole program. No reflection metadata is emitted, and none is needed.
   A reference to a DEPENDENCY property, to an extension property, or to one with context
   parameters still declines by name: each needs a receiver shape this object does not carry.

   **Delegated properties rode in on that, for 2361 pass — the largest single step yet.** The object
   a delegate is handed so it can ask `getValue(thisRef, property)` about the property is exactly
   the reference `C::x` is, so a member's delegation needed nothing new. What it needed was for the
   class-owned static holding that metadata to be allowed at all: an owner is a placement fact that
   on the JVM says WHEN the initializer runs, and here it says nothing about THIS initializer,
   because a property reference with no bound receiver is one object per property with no state and
   nothing to allocate — the same value however early it is asked for, which is the same reason a
   `const val`'s owner says nothing. Any other class-owned initializer still declines.
   A LOCAL delegated property took one small addition: it has no storage and no accessors, and
   Kotlin gives its metadata no receiver to read through, so its type answers `name` and puts the
   runtime's abstract trap in the two slots no type a program can name there declares.

   **Extension property references followed, for 2379.** The object carries at most one receiver,
   and which kind it is turned out not to matter to it: `x::p` binds a class's and `"ab"::ext` an
   extension one, and either way it is the single operand the accessor leads with. What the site
   cannot realize is a property whose accessor wants MORE than one — a member extension property has
   two receivers and a context property has operands beyond them — and the property's own LAYOUT is
   what says which it is, rather than the `extension_receiver` flag on the node, which is a fact
   about the accessor's parameter list and not about this object.

   **The unsigned integers came next, for 2394.** They had been declined whole since the first
   scalars landed, and the reason is worth keeping: common lowering erases each of the four value
   classes to the signed machine integer it wraps, so carrying one as that integer is right about
   every bit and wrong about every question — `4294967295u` and `-1` are the same 32 bits, and a
   backend that cannot tell them apart answers `1u as? Int` with `1`. What made them answerable was
   `ir.logical_types`: the checked type still sits beside each expression after the erasure, so the
   generator can read the bits the way the program meant them. Most of the work then turned out to
   be deciding, member by member, whether the machine already agrees — `plus` and the bitwise
   operators do, because two's complement makes the result the same bits; `compareTo`, `div`, `rem`
   and `shr` do not, and each takes the unsigned instruction; `toString` leaves the machine
   entirely. The two narrow widths needed one thing more: they arrive already widened into an `Int`,
   so `UShort.MAX_VALUE` reaches the generator as `Const(Int(-1))` and has to be reduced back to the
   width its checked type claims before anything reads it. Four runtime descriptors beside the
   signed ones finish the separation, which is what the JVM's cross-check caught first: a boxed
   `UInt` that the runtime's structural equality did not recognize compared unequal to itself.

   **Top-level delegated properties came free with a smaller reading, for 2411.** A property
   reference was reaching a top-level property through its slot, so a property that HAS no slot —
   one with source-written accessors, one delegated — had no reference at all, and a top-level
   delegated property needs one before any `::` is written, since the metadata handed to
   `getValue(thisRef, property)` is that object. The extension case had already built the right
   path (reach the value through the accessor pair, not through storage) and had been written down
   as a fact about extensions; it is really a fact about properties whose value is not in a slot,
   and the only thing extensions add is that their accessor leads with a receiver. Saying that
   instead — one flag, no new mechanism — took 47 declined files to 30.

   **Then master's own bottom-value contract, for 2427.** Common lowering had just gained a node
   for the one thing `Nothing` does not cover — a producer whose Kotlin type says there is no value
   and which returns anyway — and the native backend, knowing nothing about it, declined 28 files
   that had been passing. Realizing it took reading the contract rather than the position: the node
   already carries WHICH of the two cases it is, decided once from the producer, so the backend
   hands a substituted generic result to whatever asked and ends the path for a genuinely divergent
   one, instead of asking again at each use site whether a value is wanted.
   Two neighbours came with it. An unbroken `while (true)` now takes its exit out of the graph
   rather than leaving it unreachable in it, which is what makes a `Nothing`-returning function
   writable at all — and that in turn let three corpus files through that had been declining, which
   promptly segfaulted: a `tailrec` whose recursion the rewrite had not actually removed. The
   rewrite is fixed in its own change (#920); what belongs here is that common lowering now says
   whether it FINISHED and not only whether it was attempted, because a self-call left in a
   loop-rewritten function is a stack overflow at exactly the depth the modifier was written to
   make safe. The generator declines those rather than emitting them.

   **A list runtime was the largest single decline left, and it took 2436.** `listOf` accounted for
   38 files, almost all of them `for (x in listOf(...))`, and the piece that was missing turned out
   to be small: a vararg call already builds an `Array<T>`, so a list is that array with a header —
   which is what Kotlin's own `listOf(vararg)` wraps too, and what makes its elements traced by a
   collector that already knows arrays. What was NOT small was deciding which calls may reach it.
   Keying on the receiver's type is the rule ranges already follow, but `Iterable` breaks it, because
   a range is an `Iterable` as much as a list is: the first version sent an inlined
   `Iterable<T>.forEach` over a range into the list helpers, which read a bound as a pointer and
   segfaulted. A receiver the generator can only type by the interface is now answered by a runtime
   dispatch on the descriptor instead — the honest place for a question no static type can settle.

   **Two smaller ones finished the collections work, for 2452.** `listOf(x)` is a different
   DECLARATION from `listOf(vararg)`, not a vararg call of length one, and finding what separates
   them took three tries: the argument's type does not (the single-element overload's parameter is
   `T`, which may itself be an array), the argument's node shape does not (`arrayOf(1, 2, 3)` lowers
   to the very vararg node a packed call would have), and only the selected declaration's PHYSICAL
   parameter does, because a vararg one is an array however its element type was substituted. `Pair`
   came next and was a list in miniature — two references, the three `kotlin.Any` members answering
   componentwise — except that it produced the first real collision of the receiver rule: a range
   declares `first` too, and the getter guard read only the name, so `range.first` went to the pair
   helper and read a bound as a pointer. The range tests caught it, which is the argument for having
   written them.

   **`by lazy` took it to 2463, and opened a direction the runtime had not gone in.** A lazy is a
   small object — initializer, value, one bit — but computing its value means CALLING the
   initializer, and the initializer is a function value the generator emitted. So for the first time
   the runtime dispatches INTO emitted code, through the one vtable slot a function value declares
   beyond `kotlin.Any`'s three. That slot is now a stated contract between the two halves rather
   than an implementation detail of lambdas, and it is what any future runtime-held callback will
   use. Only the plain one-argument `lazy` is answered: the overloads taking a thread-safety mode or
   a lock decline by arity, because this target has no threads and quietly treating one as the plain
   form would drop exactly what the program asked for.

   **Then `map`/`forEach`, which cost a miscompile to land — 2472.** Building them was easy once the
   runtime could call a function value, and the one real decision was the same one iteration had
   already faced: both are declared on `Iterable`, which a range wears as much as a list does, so
   they dispatch on the descriptor rather than on any static type. What was NOT easy is that they
   made one more corpus file compile, and that file then answered wrongly — for a reason that had
   nothing to do with collections and had been latent all along.
   A `var` a closure captures is a cell, and which parameters carry one was being READ OFF THE BODY:
   a body that dereferences a holder is holding one. That misses the body which only passes it on —
   a lambda that hands the cell to an object it constructs dereferences it nowhere — so the thunk
   loaded a pointer as an `i32`. Common lowering had recorded the fact all along, in
   `shared_capture_parameters`, and the generator was inferring instead of reading it.
   The presentation is the part worth remembering: the truncation is invisible where it happens. The
   reference path still read the cell and rendered the right number while every scalar use of the
   same variable read a different one — `"" + x` said `1` and `x == 1` said false, in one
   expression. Narrowing it took an array index, which is a use of the value that neither renders
   nor compares, and then the emitted Cranelift function, which is what the new `native` trace
   category exists for.

   **The cheapest increment of the lane so far, for 2484.** A `fun interface` whose single abstract
   method is an EXTENSION was declined by a flag — `has_receiver` — that nothing downstream needed:
   the receiver is a declared parameter of the interface method, so the thunk was already forwarding
   it with every other argument. Deleting the condition was the whole change, and 12 files passed.
   Worth recording as a kind of decline to go looking for: one that tests a PROPERTY of a construct
   rather than a capability the code lacks.

   **Spread arguments, for 2496.** The one thing a `vararg` call had been able to assume is that it
   can count its elements, which is what gives each a constant offset. `f(a, *xs, b)` takes that
   away: the length is summed at run time and the elements go at a running index, a spread copying
   its elements in and the rest placed one at a time. The copy is not an implementation choice but
   the semantics — the callee's array is its own, and one that shared storage with the caller's
   would let a write reach back through it, which is what the third test pins.

   **The primitive iterators, for 2615.** Six more that the walk above made reachable and did not
   finish: `ByteArray.iterator()` is a `ByteIterator`, a concrete class the role table did not know,
   so `hasNext` on it declined. Adding the five that have no range — Byte, Short, Boolean, Float,
   Double — and the narrow `nextX` spellings beside `next` was the whole of it. The three that DO
   have a range stay out of that table on purpose: a range's iterator wears them and is read by the
   narrow protocol first.

   **Walking an array or a string, for 2609.** Ten more, and only three of them were `withIndex`:
   giving these two an iterator means every `Iterable` member reaches them through the dispatch
   that was already there. The element read is the one thing that cannot be shared — an array's
   descriptor is the only thing that knows the element's width and how to box its bits, and a
   string's units are not what its storage holds.

   The full e2e run caught what the conformance lane did not: a program that had been DECLINING
   (`intArrayOf(1).iterator()`) started being emitted, and emitted wrong. Its static type is
   `IntIterator`, whose `next` carries a number rather than a reference, so the new walk was being
   read as a range. Two protocols reach the same object and the descriptor is what separates them —
   which is the rule this runtime already follows everywhere else, and which I had applied to only
   one of the two entry points.

   **`withIndex`, for 2599.** Lazy rather than eager, which cost nothing to do properly: the
   object keeps the source and the iterator it hands out counts as it walks, so a loop that breaks
   never asks for the rest. The four that came back were the `Iterable` sources; the array and
   `CharSequence` ones wanted an array iterator and a string iterator, which is the increment
   below.

   **A stale decline, for 2595.** A data class holding a `Double` had been declined with a reason
   that had stopped being true: "the runtime cannot render the value". It can — `krusty_fp.c`
   landed since, and `"${1.5}"` had been answering `1.5` for a while. The whole increment was
   deleting a guard and writing the two lines it was standing in for. Worth remembering that a
   decline carries a REASON, and a reason can go out of date while the decline stays.

   **`super` on a property, for 2590.** The decline read "a `super` call to an unknown method",
   and the method really was unknown — because it does not exist. A property with default accessors
   contributes no method to its class, so the search had nothing to find, and the eighteen corpus
   cases behind it were all `super.p` rather than `super.f()`. The fix is the definition: the named
   class's own accessor when it wrote one, its own field otherwise, and never the dispatch slot —
   which is the override that wrote `super.p` in the first place.

   **Class literals, for 2576.** Every type already carries a descriptor with its Kotlin name, so
   a `KClass` is that pointer with a header on it, and equality is the pointer. Two things came out
   of it. The first was a link failure I nearly missed: `kt_class_for` is already the collector's
   size-class helper, and a freestanding program links one namespace — the build WARNED and turned
   every native target off, which silently turns every native test into a skip, so the probes that
   "passed" had not run at all. Read the build warnings.
   The second is a shared defect this increment made visible rather than caused:
   `sam/constructors/sameWrapperClass2.kt` wants two SAM conversions of one lambda to be instances
   of ONE wrapper class, and both backends emit a type per conversion SITE. It was invisible while
   `::class` declined. Ledgered with the JVM's identical failure; fixing it is common lowering's,
   keyed on (interface, implementation, capture shape) rather than on the site index.

   **`TODO`, `error`, `require`, `check`, for 2570.** These throw, and this target has no
   exceptions — but it already answers `!!` on null and a failed cast with a diagnosable exit, and
   these are the same thing with the program's own message. Nothing is decided about `try` by
   doing so: no program that could catch one of these compiles here at all, so there is no
   observable difference to get wrong, and when exceptions arrive these become throws.
   The forms taking a `lazyMessage` stay declined, and that is the considered part: the parameter
   is a lambda of an `inline` declaration, Kotlin lets such a lambda return from the enclosing
   function, and nothing here can splice a dependency's body — so calling it as an ordinary
   function value would be a miscompile, not a slower answer.

   **The collector against the runtime's OWN objects.** The stress tests covered what the
   generator emits — class instances, arrays, closure captures — and nothing that the runtime lays
   out itself. That gap had grown: a list, a pair, a lazy, a list iterator and a shared string
   storage each carry their references in a hand-written descriptor, and a wrong offset there is
   the same bug with none of the same coverage. Four programs now hold each of them across many
   collections and read them back. Each was checked by BLINDING the descriptor it tests — dropping
   the reference count, or pointing an offset at the wrong field — and confirming the test fails;
   two came back as wrong answers and two as a SIGSEGV, which is what a wrong offset does.

   The concurrency half of the same question needs no new tests yet, and that is a finding rather
   than a gap: this runtime starts no threads, and `the_runtime_starts_no_threads` pins that by
   reading the syscall header rather than by trusting a comment. It fails the moment `clone`,
   `futex` or `pthread` appears — which is exactly when the Kotlin/Native memory model this target
   committed to has to be implemented and these tests rewritten.

   **`joinToString()`, for 2561.** The interesting part is what is NOT realized. Every parameter
   of it is defaulted, and a dependency's defaults live in a `$default` synthetic this backend
   cannot call — so supporting the argument forms would mean writing the stdlib's default values
   into the generator, where they would quietly rot. Passing nothing is the one set worth writing
   down, and every corpus case behind this wanted exactly that.

   **Slicing and ordering a string, for 2551.** All of it is a walk, because the storage is UTF-8
   and the index is a UTF-16 unit — the same walk `length` and `get` already pay for. The one thing
   that is not a walk is `removeSuffix`, where bytes settle it outright. Two spellings caught me
   here: these extensions arrive as members of the `kotlin/text` FACADE rather than of
   `kotlin.String`, and the provider presents a property's accessor under the property's Kotlin
   name (`length`) rather than the JVM one (`getLength`).

   **A primitive reached as an object, for 2538.** `Number.toInt()` and `x++` on an `Int?` look
   like one problem and are two: the first has to read the descriptor because the site knows only
   `Number`, the second must not, because the owner the member was selected on already says. The
   first draft did the same thing for both — box the receiver, then unbox it at the owner's type —
   and that is how `Long.MAX_VALUE.inc()` answered zero: the box was written by the source's type
   and read by the target's, and nothing objected when they disagreed.

   **A box cache that lost the top half of a `Long`.** Found while bringing up `inc`/`dec` on a
   boxed primitive: a corpus case answered `Fail decLong`, and the reduction had no `inc` in it at
   all — two interpolated `Long`s were enough. The cache slot was picked with `(int)value`, so
   every `Long` whose low word happened to land in -128..127 took that small number's slot and came
   back as it. A long-standing wrong answer that nothing had asked for before, because printing two
   `Long`s where one is `Long.MIN_VALUE` is a narrow thing to do.

   **Delegating to a reference, for 2522.** `val x by ::top` calls one of four stdlib operators
   that are `inline` one-liners over the reference's own `get`/`set`. Nothing can splice a
   dependency's `inline` body, so they are realized here — the only real question being which
   overload a site took, which the receiver's type answers: `KProperty1` is handed a receiver and
   `KProperty0` is not. Fourteen corpus cases, well past the five the decline table named, because
   the same programs were behind the declaration-order fix above.

   **Declaration order, for 2508.** `class A { val r = C::z }` was declining on a construct the
   generator has realized for a long time. The reference passes ran between `define_classes` and
   the top-level function loop, which reads as "after the declarations, before the bodies" and is
   not: a constructor is a body, and a class's property initializers are lowered into it. Five
   corpus cases came back from moving two lines.

   **`KCallable.name`, for 2503.** A callable reference is a lambda object on this target, and a
   lambda does not know its own name. It does not need to: the only question a program can ask is
   `::foo.name`, and there the reference is written at the read, so the declaration is in hand and
   the name folds to a constant. Getting it right was a matter of finding where the source name
   survives — the emitted adapter's is mangled (`$fir_callable_ref_1_2`), while
   `ir.referenced_module_callables` keeps the Kotlin name precisely so a reference's identity does
   not depend on what the adapter ended up being called.

   **Range membership, for 2501.** `x in a..b` had been declining although ranges were built long
   ago, because the checker does not hand it a range at all — it hands the bounds, so the answer is
   two comparisons and no object. Writing the effect-order test taught me something I had wrong:
   the subject is evaluated LAST, not first, because `x in a..b` is `(a..b).contains(x)` and a
   receiver precedes an argument. The test asserted my guess, failed, and the cross-check against
   the JVM settled which of us was right.

#### Decided: Kotlin/Native's memory model, not the JVM's

The native target reproduces **Kotlin/Native's** concurrency contract, not the JVM's. That follows
from the framing this whole track sits under — a Kotlin Multiplatform native target, with no Java
interop — and it is not a compromise: requiring the JVM memory model would be a *stronger* guarantee
than Kotlin/Native itself offers, so code that is correct on Kotlin/Native would remain correct
here.

What Kotlin/Native actually specifies, and therefore what has to be emitted:

* **`@Volatile` (`kotlin.concurrent.Volatile`)** — reads and writes of the backing field are atomic
  and writes are visible to other threads. Note the exact scope: only *backing-field* operations
  are atomic, so a property whose accessor touches the field several times is not atomic as a whole.
  The emitter has volatility as a declaration fact already, so this is emitting an atomic access
  instead of a plain one, not inferring anything.
* **`kotlin.concurrent.atomics`** (`AtomicInt`, `AtomicLong`, `AtomicReference`) — compare-and-swap
  and atomic update, mapped to the host's atomics.
* **No `synchronized`.** It is a JVM-only construct; it does not exist on Kotlin/Native, so there is
  no monitor to implement.
* **No final-field freeze.** The JVM's `final`-field safe-publication rule is not part of
  Kotlin/Native's contract, so there is no release fence to emit at constructor exit.

Those last two are the whole of what this decision removes, and they were the expensive half. It
also removes the one item Go could not supply: Go's memory model has no final-field freeze, which
was listed above as its specific gap — under Kotlin/Native's contract there is nothing there to
reproduce.

**The collector lands in the same place.** Kotlin/Native's GC is a *concurrent mark-and-sweep,
non-generational* collector (with a parallel-mark/concurrent-sweep fallback). Go's is a concurrent
mark-and-sweep, non-generational collector. Emitting Go would not approximate Kotlin/Native's
runtime here — it lands on the same collector design, for free. Kotlin's own documentation describes
its native memory manager as "similar to the JVM, Go, and other mainstream technologies", naming Go
directly.

Emitting C still cannot reach it, for the structural reason above: no stack maps, so conservative
scanning is the ceiling. With the memory model settled this way, the collector is the binding
constraint on the C path and nothing else is close.

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

The C backend already runs under that relaxation in its smallest possible form: every native test
compiles a program, links it, executes it and compares its OUTPUT. No test inspects generated C — a
C program that reads correctly and prints the wrong thing is exactly what a code-shape assertion
cannot catch.

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
switching. Note that emitting a *host language* answers all three by inheritance rather than by
design: C forces conservative GC (see above), and Go would supply its collector, `panic`/`recover`
and goroutines. That is most of the argument in Go's favour and the whole of the reason not to let
the C scaffold drift into being the answer.

**The C backend must not become the answer by inertia.** It was built to keep the code-generator
question open, and a working thing has a way of settling questions no one meant to settle. C cannot
express what a real native target needs — precise GC stack maps, a chosen exception mechanism,
stack switching for `suspend` — so the low-IR decisions below stay open, and the C emitter stays a
scaffold with an expiry date rather than a target to grow features on.

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
