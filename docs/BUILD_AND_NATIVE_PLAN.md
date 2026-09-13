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

#### Landed: a native backend that emits C

`src/native/` compiles checked common IR to C and links it with `cc`. `fun main() { println(…) }`
builds to an executable that runs with `JAVA_HOME` and `PATH` emptied, and so do arithmetic, locals,
`while`, a lowered `for`, recursion, string concatenation and string templates
(`tests/native_hello_world_e2e.rs`).

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
* **The C emitter is retired.** `src/native/emit.rs` and the emitter half of `classes.rs` — about
  3,600 lines — lowered common IR to C text. They go, replaced by a lowering to machine code. The
  five class slices landed against them are not wasted: their tests are Kotlin programs with
  expected output, and every one of them becomes a test of the new code generator the moment it can
  run them. Their runtime and object-model halves stay.
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
3. Re-run the landed class tests against the new generator, slice by slice, until they all pass.
4. Per-module objects and ABI-hash caching — the incremental half.
5. The `codegen/box` corpus through the native pipeline as the conformance gate: skipping permitted,
   miscompiling never; declined reasons sorted by frequency are the backlog.

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
