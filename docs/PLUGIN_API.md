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

Phase separation is a **convention, not a structural guarantee**: every hook gets `&mut IrFile`, so
nothing stops `generate_declarations` from rewriting a body. This mirrors kotlinc (FIR/IR extensions
also get broad access) and is deliberate — the phase ordering, not a capability sandbox, is the
contract. A future tightening could pass a phase-scoped facade instead of raw `&mut IrFile`.

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

- a nested `Foo$serializer` **object** implementing `kotlinx/serialization/KSerializer<Foo>` with
  `getDescriptor`, `serialize`, `deserialize`, `childSerializers` (decl phase);
- a static `serializer()` accessor whose body reads the `Foo$serializer.INSTANCE` singleton (decl
  phase; kotlinc places it on `Foo.Companion`);
- `childSerializers` filled (body phase) with a **real array of one element serializer per property**
  — its length tracks the field list and each element names that field's serializer
  (`kotlin/Int` → `…builtins/IntSerializer`, a nested `@Serializable` type → its own `$serializer`);
- a `write$Self` helper whose **name is version-dependent** (see Versioning below).

`serialize`/`deserialize` bodies are placeholder `return`s in the PoC — in production they call the
**real published `kotlinx-serialization-core`** runtime (`Encoder`/`Decoder`/`SerialDescriptor`).
Only codegen is native. This is the template for porting any FIR/IR plugin.

#### Versioning — codegen follows the target runtime

The serialization plugin takes a target ABI version (`SerializationAbi`) + module name, exactly as
krusty itself is pinned to a kotlinc version. The synthesized member shape changed across releases —
e.g. the per-class write helper is unmangled `write$Self` on core `< 1.6` but module-mangled
`write$Self$<module>` on core `>= 1.6` (Kotlin `>= 1.8.20`). Generated code that doesn't match the
linked runtime is a **runtime linkage error**, so version-aware codegen is mandatory, not cosmetic.
The PoC demonstrates the branch on the write-helper name; a full port carries a codegen profile per
supported runtime version, validated by the differential harness against each.

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
Dagger/Micronaut codegen where one generated type triggers another processor. A `with_max_rounds`
backstop caps a non-converging chain deterministically (committing each round's work, then stopping).

### Versioning — KSP is tied to the kotlinc version

KSP ships **per Kotlin compiler version**: artifacts are coordinated `<kotlin>-<ksp>` (e.g.
`2.0.21-1.0.28`), and KSP's behavior depends on the compiler it embeds. So the sidecar toolchain is
*determined by* the kotlinc version krusty targets — not chosen independently. The build resolves the
pair from its dependency manifest and pins the host (`KspHost::for_toolchain`); krusty bakes **no**
kotlin→ksp table (same no-hardcoded-lists rule as the rest of the compiler). The host only guarantees
the spawned sidecar uses exactly that pair, and `KspToolchain::is_consistent` catches a manifest where
the KSP coordinate isn't prefixed by its Kotlin version before a mismatched sidecar emits wrong symbols.

### Interaction model — how the host drives real JVM processors

krusty is Rust; a real processor is JVM bytecode needing a live JVM + the KSP API. A JVM **sidecar**
is mandatory; the question is how thick the bridge. Two strategies:

- **A. Orchestrator** — the sidecar runs **real KSP + the Kotlin Analysis API over the same source**
  krusty compiles; krusty only spawns it, points it at the source + classpath, and ingests the
  generated sources (re-parse → fixpoint). **Zero type-system reimplementation, full fidelity.** Cost:
  the JVM re-parses source once (for KSP only, not codegen).
- **B. Native shim** — a shim JAR implements KSP's `Resolver`/`KSClassDeclaration`/`KSType` backed by
  krusty's serialized symbol model (this is the boundary the PoC models). No re-parse, but krusty must
  reimplement enough Kotlin **type-system** ops (`isAssignableFrom`, variance, type args) to satisfy
  processors — the exact divergence-from-kotlinc risk krusty fights elsewhere, and it needs the full
  generic type model first.

**Decision is driven by the build mode:**

| | CI (one-shot, cold) | Dev loop (repeated) |
|---|---|---|
| warm daemon / incremental cache | useless (process dies, cold checkout) | big win |
| orchestrator double-parse | paid once → fine | per-build cost |
| recommendation | **A (orchestrator)** | A, or B if double-parse latency hurts |

CI is the primary target → ship **A**. Cold-run cost is `JVM_start + max(krusty_compile, KSP_run) +
recompile_generated`; attack it with: a shipped **CDS/AOT archive** of the fixed KSP+analysis stack
(dominant cold cost; GraalVM native-image is out — processor JARs load at runtime via ServiceLoader +
reflection), **overlapping** KSP with krusty's own compile, an **annotation-presence gate** (never
spawn the JVM when no registered processor's trigger annotation appears), and **native frameworks
bypassing the JVM entirely** (serialization/Compose are native passes — only true APs spin a sidecar).
Drop daemon/incremental/zero-copy machinery for CI; it's dev-loop tooling.

## Drop-in extension management — two layers, like kotlinc

Extensions have **two registration layers**, mirroring kotlinc exactly:

1. **Extension registration (general)** — `plugins::registry::PluginRegistry`. Records which compiler
   plugins krusty knows about, independent of any compilation — the analogue of a plugin's
   `CompilerPluginRegistrar` declaring its extensions to the compiler. Each known plugin maps a
   kotlinc plugin id to either a **native** reimplementation (`IrPlugin`, run in-process) or a
   **codegen host** (KSP, run via sidecar). `with_builtins()` ships serialization (native) + KSP
   (host); `register()` is open, so a third party can add a native extension (proven by test).
2. **Per-compilation activation** — `plugins::cli::PluginConfig`. The **exact switches kotlinc uses**,
   so an existing Gradle/Maven build works unchanged:

   ```
   -Xplugin=/…/kotlinx-serialization-compiler-plugin.jar
   -Xplugin=/…/symbol-processing.jar
   -P plugin:com.google.devtools.ksp.symbol-processing:apclasspath=/…/processor.jar
   -P plugin:com.google.devtools.ksp.symbol-processing:kspOutputDir=build/generated/ksp
   ```

`PluginRegistry::resolve(Activation)` **joins them** — registry × this unit's switches → the plugins
to run. Drop-in rules it enforces:

- **Activation = a jar on `-Xplugin`** (or `-P` under the plugin id), never a krusty flag.
- **Versions are not flags.** Serialization's ABI comes from the `kotlinx-serialization-core` jar on
  `-classpath` (`SerializationAbi::from_classpath`); KSP's from its jar coordinate (`KspToolchain`,
  tied to the targeted kotlinc version). Same inputs as kotlinc → same codegen.

### Reliable diagnostics (drop-in safety)

`resolve` returns `diagnostics: Vec<PluginDiagnostic>` the driver forwards to the `DiagSink`. Because
silently dropping a plugin would emit wrong bytecode, each activated plugin gets an explicit verdict:

| Situation | Diagnostic | Severity |
|---|---|---|
| native reimpl (serialization) | `NativeSubstitution` — krusty runs its own ABI-matched impl; the supplied jar is **not** executed | INFO |
| hosted (KSP) | `Hosted` — the real jar runs via the sidecar | INFO |
| unknown `-Xplugin` jar (Compose, any third-party FIR/IR plugin) | `Unsupported` — krusty can neither run nor substitute it | **ERROR** (fails the compile) |

So a build that pulls in Compose fails loudly with a clear message ("remove the plugin or compile this
module with kotlinc") instead of producing a silently-broken artifact, and a serialization build is
told plainly that krusty substituted its own implementation for the original plugin.

## Reusing kotlinc's own plugin tests for conformance

krusty's correctness is defined differentially vs real `kotlinc` (`docs/SPEC.md`), and that extends to
plugins — **reuse the upstream suites rather than writing fresh ones** (both are Apache-2.0):

- **kotlinx.serialization** — the compiler plugin ships a box-test corpus at
  `plugins/kotlinx-serialization/testData/boxIr/*.kt` in the Kotlin source tree (present locally under
  `external-projects/kotlin-2.4.0/…`). These are `fun box(): String` round-trip tests in **exactly the
  format krusty's existing box harness (`tests/kotlin_box_ir_jvm_conformance.rs`, `just box-corpus`)
  already runs**. Point the harness at that directory, link the real `kotlinx-serialization-core/-json`
  runtime (the jars are in the local gradle cache), and a passing `box()=="OK"` is end-to-end proof the
  synthesized `$serializer` is correct. NOTE: requires the real `serialize`/`deserialize` bodies (the
  PoC stubs them), so this is the conformance *path*, lit up once bodies land — not green today.
  Available now with the same harness: a **bytecode/ABI diff** of krusty's `$serializer` vs kotlinc's.
- **KSP** — KSP's `test-utils` golden-symbol tests feed source to a test processor and assert on the
  resolved symbol model it observes. Under the **orchestrator** strategy they pass by construction
  (real KSP runs). Under the **native shim**, they are precisely the conformance suite for krusty's
  `Resolver` fidelity (does `getSymbolsWithAnnotation` / type resolution match kotlinc?). Downstream
  framework suites (Dagger, Room) run on the host as black-box e2e.

The same `box()` corpus the rest of krusty is gated on absorbs serialization for free — no new harness.

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

## Implementation status (rounds 1–6) and honest gap to the conformance bar

Landed on `master`, each round reviewed (cavecrew-reviewer), TDD:

| Capability | State | Where |
|---|---|---|
| Extension surface (`IrPlugin`, `PluginHost`, phases) | done | `plugins/mod.rs` |
| Two-layer registration + kotlinc `-Xplugin`/`-P` activation | done | `plugins/registry.rs`, `plugins/cli.rs` |
| Reliable diagnostics (native-sub / hosted / **unsupported=error**) | done | `plugins/registry.rs` |
| Serialization native plugin: `$serializer`+members, `serializer()`, per-field `childSerializers`, ABI-from-classpath | done (decl/shape) | `plugins/serialization.rs` |
| Annotation capture from source → plugin activates end-to-end | done | `parser.rs`, `plugins/mod.rs`, `tests/plugins_e2e.rs` |
| KSP/codegen host model + fixpoint + toolchain pinning | done (model) | `plugins/ksp.rs` |
| Real annotation-processor-**from-jar** run (codegen-host mechanism) | done (APT) | `tests/codegen_host_e2e.rs` |
| Toolchain **provisioning** (detect gradle/mvn/cs → fetch jars to folder) | done | `plugins/deps.rs`, `tests/ksp_provision_e2e.rs` |
| **Real KSP2 run from a JAR**: provision → compile Kotlin processor → `KotlinSymbolProcessing.execute`; ServiceLoader from jar, annotation query, property inspection, **multi-round**, **generated code compiled** | done | `tests/ksp_real_e2e.rs`, `tests/fixtures/ksp/` |

The **KSP** clause of the goal is met: `tests/ksp_real_e2e.rs` runs a real KSP processor loaded from
a JAR via the actual KSP2 toolchain and verifies the full case matrix — from-jar discovery, annotation
query, declaration/property inspection, multi-round re-processing of generated code, and that the
generated code itself compiles. (Opt-in `KRUSTY_KSP_E2E=1`; runs under a JDK ≤ 23 since Kotlin
2.0.21's compiler rejects JDK 25.)

**Remaining distance to the stated bar — serialization conformance only:**

The extension surface is sufficient (it synthesizes the serializer); serialization conformance is
blocked **downstream of the surface** by core compiler capability. Proven concretely (round 8) by
trying to compile a HAND-WRITTEN `KSerializer` with krusty (`tests/fixtures/serialization/
ManualSerializer.kt`) — it fails on three core gaps, none about the plugin:

1. **object self-reference** — `object S { … S … }` failed to resolve `S` inside its own body; a
   `$serializer` references its own `INSTANCE`. **FIXED (round 9, gate-verified 0 FAIL):** a bare
   object name used as a value now resolves to the singleton.
2. ~~constructor overload matching with a `null` argument~~ **FIXED (round 10, gate-verified):**
   `PluginGeneratedSerialDescriptor(name, null, count)` now compiles (`LibraryType::ctor` matches a
   `null` arg against a reference parameter). (`ArrayList()` already worked with the JDK jimage.)
3. **`Json` round-trip resolution cluster** — several classpath resolution features, each resolve+emit:
   (a) static-field access on a classpath class (`Json.Default` itself fails to resolve);
   (b) companion-instance dispatch (`Json.encodeToString(serializer, value)` → call on the `Default`
   singleton, inherited from `StringFormat`); (c) companion extensions (`Int.serializer()`).
4. **the serializer object** — implements a generic interface (`KSerializer<Foo>`), has a `descriptor`
   property initializer, and a `decodeElementIndex` state-machine loop. NOTE: a **plugin-generated**
   serializer is built at IR level, so it BYPASSES the `is_simple_object` AST-lowering limit — the
   plugin must still emit correct IR bodies and be wired into the emit path.

Progress this session: blockers #1 (object self-ref) and #2 (ctor null-match) are **closed and
gate-verified (0 FAIL)**. Further probing surfaced MORE independent compiler gaps the serializer
needs, confirming this is multi-session core-compiler work:

5. **FQ resolution of an ambiguous-simple-name type** — `scan_types` drops a simple name that maps to
   multiple classpath internals (several `Encoder`/`Decoder` exist across the serialization jars), and
   type refs resolve via that simple-name map, so even fully-qualified
   `kotlinx.serialization.encoding.Encoder` fails to resolve (a JDK interface like `Runnable` and the
   concrete `PluginGeneratedSerialDescriptor` both resolve — it's specifically the ambiguous ones).
6. classpath **static-field read** (`Json.Default`), **companion-instance dispatch**
   (`Json.encodeToString`), **companion extensions** (`Int.serializer()`) — the Json cluster (#3),
   side-steppable via a real-kotlinc round-trip driver.
7. the **plugin codegen** itself (#4): emit a functional `$serializer` object implementing the generic
   `KSerializer` interface — `clinit`-built descriptor, encode calls, decode state machine, erased
   bridges — and wire `PluginHost` into the emit path.

Each of #5–#7 is a separate resolver/backend feature requiring its own 1303-test gate verification.
The conformance bar ("all 69 boxIr round-trips") additionally needs sealed/polymorphic/generic/
inline-class/contextual language support. This is a multi-day, multi-round track, not a single session.

### CORRECTED critical path (key insight)

The serialization plugin synthesizes the `$serializer` as **IR with full internal type names**, so it
**never goes through krusty's source type-resolver**. That means the plugin path **bypasses** gaps
#1, #2, #3, #5 — those only block compiling serializer *source* (a hand-written serializer, or FQ
type refs, which krusty's `ty_of_ref` also can't resolve). Rounds 9–10 (object self-ref, ctor
null-match) are real, gate-verified general improvements but are **not on the plugin's critical path**.

### STATUS UPDATE — serialization full ENCODE+DECODE round-trip is GREEN

Gap #7 is **closed for the flat case (both directions)**: krusty compiles `@Serializable class
Foo(val a: Int, val b: String)`, its plugin emits a **fully functional** `$serializer`, and a real
`kotlinc`-compiled driver round-trips it both ways against the **published `kotlinx-serialization`
runtime** — `Json.encodeToString(Foo.serializer(), Foo(1,"x"))` → `{"a":1,"b":"x"}`, and
`Json.decodeFromString` of both that and a non-default `{"a":42,"b":"hi"}` reconstructs the values.
Executed + verified green (`tests/serialization_roundtrip_e2e.rs`, `KRUSTY_SER_E2E=1`), box gate 0-FAIL.

#### Update — FULLY-IN-KRUSTY bidirectional round-trip (no kotlinc anywhere)

The round-trip now compiles **entirely in krusty**, driver included: `Json.encodeToString(Foo.serializer(),
Foo(7,"hi"))` THEN `Json.decodeFromString(Foo.serializer(), j)` then reads `back.a`/`back.b` — krusty
emits all of it, the JVM runs it against the published runtime, result `"hi7"`
(`tests/serialization_krusty_only_e2e.rs::serializable_class_round_trips_through_json_entirely_in_krusty`).

The decode half needed **front-end generic-return inference** for a classpath member whose return erases
to `Any`. `Json.decodeFromString` is a *member* `<T> T decodeFromString(DeserializationStrategy<? extends
T>, String)` — its return is the type variable `T`, erased to `Any`, so `back.a` failed as "unresolved
member on Any". Fix: at the companion-instance call site, when the resolved member's return is the erased
`kotlin/Any`, run `LibrarySet::instance_call_return` — it finds the member up the receiver's hierarchy
(accepting a SUBTYPE argument `KSerializer<Foo>` for the `DeserializationStrategy` parameter), unifies the
method's generic parameter signatures against the actual argument types (`unify_gsig` zips type arguments
positionally, so `KSerializer<Foo>`'s `<Foo>` binds `T` despite the parameter's different class), and
substitutes the generic return → `Foo`. The `Any`-only guard is load-bearing: a concrete return
(`encodeToString: String`) must keep its canonical `Ty::String`, not the re-derived `Obj("kotlin/String")`.
`serializer()` already returns `KSerializer<Foo>` carrying the type argument. Box gate 1716/0, additive.

What the plugin emits (and krusty's emitter accepts):
- the `$serializer` object implementing `KSerializer` + erased generic bridges;
- a `<init>`-built `PluginGeneratedSerialDescriptor` (`.addElement` per property), `getDescriptor`;
- `serialize`: drives the `CompositeEncoder` (`beginStructure`/`encode<T>Element` via each property's
  **public getter**/`endStructure`);
- `deserialize`: a real decode state machine — `beginStructure` → `while { i = decodeElementIndex;
  if i==-1 break; if i==k f_k = decode<T>Element(k) }` → `endStructure` → construct from field locals;
- `serializer()` accessor.

(The earlier "multi-session impossible" framing was over-pessimistic: the real-kotlinc-driver split
removed the Json gap and the emit primitives were already present. The verifier surfaced and we fixed
real bytecode bugs — abstract-forcing empty bodies, unreturned block values, private-field access vs
getters, ctor-receiver typing, local-slot declaration order, Unit-When statement discard.)

Update: the plugin is now **wired into the main compile path** (`krusty -cp <runtime> Foo.kt` emits a
working serializer in a normal invocation; box gate 0-FAIL), and the round-trip generalizes across the
primitive set + arbitrary field count (`Foo(Int,String)`, `Rich(Int,Boolean,Float,String)`).

Remaining for *full* conformance splits into two very different efforts:
- **Incremental serializer codegen** (on the proven foundation): 2-slot types (Long/Double),
  nullable/nested/richer types + real `childSerializers`/`decodeSerializableElement`.
- **The literal 69-case `testData/boxIr` corpus is NOT flat round-trips — it is an edge-case suite.**
  Surveyed: the *smallest* files use `suspend () -> Unit` property defaults, `@Polymorphic`,
  star-projections, contextual serializers, generics, sealed hierarchies. krusty cannot compile the
  `@Serializable` *classes themselves* (those language features) before any serializer logic runs. So
  "all 69 pass" is gated on krusty's **broad Kotlin language support** (suspend/polymorphism/generics/
  contextual/sealed) — months of core-compiler work, **independent of the extension surface**. Running
  the corpus also needs either gap #3 (krusty compiling the `Json` `box()` drivers) or a per-file
  split-compile harness.

The working encode+decode round-trips prove the **extension surface and serializer codegen are
sufficient** — the goal's "show the surface is enough" is demonstrated by two extensions with real
executed tests (KSP-from-jar + functional serialization). The 69-corpus "all pass" is a separate
language-coverage milestone, not an extension-surface gap.

### MEASURED conformance survey (2026-06-24, post encode/decode/companion + member-return inference)

Ran krusty's front end over all 69 `boxIr` files (single-file subset; multi-`// MODULE:`/`// FILE:`
skipped) with `kotlin-stdlib` + `kotlinx-serialization-{core,json}` + JDK modules on the classpath.
**9/69 now COMPILE** (front end) — up from the old audit's **0/69**, attributable to companion-instance
resolution, the encode/decode plumbing, and the erased-generic-member-return inference landed this arc:
`constValInSerialName, contextualByDefault, inlineClasses, intrinsicsNonReified,
intrinsicsStarProjections, multiFieldValueClasses, polymorphic, starProjectionsSealed, uuidSerializer`.

Of those 9, runtime `box()` status: **`constValInSerialName` now PASSES** (the first fully-green corpus
file) — `@SerialName` support landed: the parser now captures annotation ARGUMENT expressions (alongside
names) on constructor properties (`Param`/`PropParam.annotation_args`), `ir_lower` const-folds the value
(`@SerialName("$prefix.bar")` with `const val prefix` → `"foo.bar"`, depth-bounded against cyclic
`const val`s) into `IrClass.serial_names`, and the serialization plugin uses it for the descriptor
element name / JSON key. The other 8 compile-OK files fail at runtime needing custom/polymorphic/sealed/
value-class serializers or reflection.

**Value-class FIELD landed → `inlineClasses` green (4th corpus file).** krusty unboxes a `@JvmInline
value class`-typed field to its underlying (`Holder.f: Foo` → `int`), so the serializer treats such a
field AS its terminal underlying (`value_class_underlying`, recursive) — `encodeIntElement` → `{"f":42}`
(same JSON as kotlinc's inline serializer), consistent with the unboxed slot. Plugin-level fix (the
runtime-only `inlineClasses` half; the `descriptor.isInline` half is the value-class serializer below).

**Value-class serializer landed** (krusty-only e2e green; a `@JvmInline value class`'s `$serializer`
uses `InlinePrimitiveDescriptor(name, <Underlying>Serializer.INSTANCE)` so `descriptor.isInline==true`,
and serialize/deserialize go through `encodeInline()/decodeInline()` — `Foo(42)` round-trips as bare
`42`). Gated to a directly-supported primitive/String underlying. The corpus `inlineClasses` file
additionally nests a value class as a FIELD of a normal class, which fails on krusty's value-class
field-representation ambiguity (boxed-vs-unboxed) — a core-compiler issue independent of the serializer.

**`@Serializable(with = X::class)` landed** (greens `contextualByDefault` + `polymorphic`, box()=OK):
`serializer()` returns an instance of the explicit serializer `X` — `new X(getOrCreateKotlinClass(C.class))`
(`ContextualSerializer`/`PolymorphicSerializer` take the class's `KClass`; their descriptors carry the
right `SerialKind`) instead of a generated `$serializer`. Plumbing: ClassDecl/`IrClass.custom_serializer`
(annotation class-literal arg, resolved via `class_names`). Two GENERAL interface bugs fixed en route: a
`static` method on an interface emits `public static` (not the illegal `final`), and a `CrossFile`
invokestatic to an interface uses an `InterfaceMethodref` constant.

**Reified serializer form landed** (krusty-only e2e green): `Json.encodeToString(x)` /
`Json.decodeFromString<C>(s)` (no explicit serializer) are `reified inline` — uncallable directly
(`UnsupportedOperationException`). `ir_lower::try_reified_serial` desugars them to the 2-arg member with
a synthesized `C.serializer()` (the way kotlinc's inliner would); `resolve::reified_type_arg` types the
decode result from the explicit `<C>`. This is the form ~20 corpus files use — but each ALSO needs a
second feature (Map/sealed/value-class/reflection/`buildJsonObject`), so no NEW corpus file greens from
reified alone; the capability is proven by `serialization_krusty_only_e2e::reified_serializer_round_trips`.

First-blocker histogram for the 38 that DON'T compile (top entries): `unresolved 'Encoder'`/`'Decoder'`
(7 — custom `KSerializer` objects, also need interface-body abstract members), `elementDescriptors`/
`getElementName` SerialDescriptor introspection (4), reflection (`typeOf`, `parameters`, class-literal
forms), annotations with array members, default arguments referencing other parameters, enum members,
contextual/`@Serializable(with=)`. Each is an independent language/stdlib feature; "all 69" remains a
multi-feature roadmap. **Nearest single win: annotation-argument capture → `@SerialName` →
`constValInSerialName` green.** (Survey is reproducible; not yet a committed test harness.)

#### MILESTONE — full `Json.encodeToString` round-trip compiles+runs ENTIRELY in krusty (no kotlinc)
`Json.encodeToString(Foo.serializer(), Foo(1,"x"))` → `{"a":1,"b":"x"}` for a `@Serializable class Foo`,
compiled AND run by krusty alone (commits 44712a6 + ab67425, test `serialization_krusty_only_e2e`). This
exercises the whole surface through krusty's own front end + backend: the plugin's `$serializer`, the
`serializer()` accessor made checker-visible by a **signature phase** (resolve.rs adds a static
`serializer(): KSerializer<C>` for `@Serializable`) + **static-call lowering** (`invokestatic
C.serializer()` by signature; the plugin fills the method before emit), the **classpath
companion-instance call** `Json.encodeToString` (= `Json.Default.encodeToString`), and **subtype-aware
overload matching** (`KSerializer` ⊂ `SerializationStrategy`). DECODE next: `decodeFromString` returns
`T` erased to `Any` — needs argument-based generic inference (`T` from the `DeserializationStrategy<T>`
arg; krusty does receiver-based substitution today, not argument-based). Neither decode nor this encode
milestone moves the 0/69 reflection/generics/sealed corpus — that remains the language roadmap above.

#### Implemented so far (rounds 12–19, all gate-verified 0-FAIL, real JVM round-trips)
Full primitive set (Int/Long/Boolean/Float/Double/String, incl. 2-slot locals) + **nested
`@Serializable` composites** (`encodeSerializableElement`/`decodeSerializableElement` with the nested
type's krusty-generated `$serializer.INSTANCE`) + **nullable elements — both reference (`String?`) and
primitive (`Int?`/`Long?`/`Boolean?`/…)** via `encode/decodeNullableSerializableElement` against the
builtin `{String,Int,Long,…}Serializer.INSTANCE`, encode+decode (present *and* `null`, e.g.
`{"a":2,"b":null}` and all-null `{"a":null,"b":null,"c":null}` round-trip), arbitrary field count,
plugin wired into the main compile path. (Nullable primitives are lowered to their boxed fq name —
`Int?` → `java/lang/Integer` — so the getter/field/local are already references; `slot_width` accounts
for boxed `Long?`/`Double?` being one slot, not two.) **Nullable nested composites (`Inner?`)** also
round-trip — the nullable variant shares the non-nullable element call's descriptor, so it's a method
name swap (`encode/decodeNullableSerializableElement`) over the nested `$serializer.INSTANCE`.

#### The classpath static-field brick — BUILT (round 19)
The previously-blocking gap ("no IR node to getstatic a *classpath* object's `INSTANCE`") is closed: a
new `IrExpr::ExternalStaticInstance { owner, ty, field }` getstatics a classpath class's static
singleton by internal name (vs `StaticInstance`, which resolves a krusty `ClassId`). It emits in
`ir_emit` (getstatic + `value_ty`); IR walkers fall through their `_` arms correctly (getstatic pushes
no stackmap frame). This unblocked nullable reference elements — the element serializer
(`kotlinx.serialization.internal.StringSerializer.INSTANCE`, verified public/getstatic-able) is now a
real reference passed to `encode/decodeNullableSerializableElement`.

#### Definitive 69-corpus audit (2026-06-24) — every file's FIRST blocker
Ran krusty over all 69 `boxIr` files (stdlib + kotlinx-serialization-{core,json} on `-classpath`).
Result: **0/69 even COMPILE** (front-end, before any box() run). First-blocker histogram:

| blocker | files | nature |
|---|---|---|
| `unresolved reference 'Json'` | **14** | bare `Json` = its companion `Default` (a `static Json$Default Default` field); krusty doesn't resolve a CLASSPATH class's companion via the bare class name (gap #3). Highest single lever. |
| `unresolved reference '<T>'` (generics) | ~10 | generic `@Serializable class C<T>` / type-param element serializers |
| `unresolved reference '<Type>'` (other) | ~8 | `PrimitiveKind`, `encodeDefaults`, user types in multi-`// FILE:` blocks |
| parser: `expected an expression / top-level decl / type / object name` | ~9 | annotation arrays, local @Serializable, `serializer()` factory forms |
| custom serializer / `@Serializable(with=…)` / contextual | ~6 | `unresolved member`/`function`, descriptor introspection |
| sealed / polymorphic | ~4 | `sealedInterfaces`, `polymorphic*`, `PolymorphicSerializer` |
| reflection / `@SerialInfo` annotation-impl | ~3 | `serialInfo`, `enumsAreCached` (EnumSerializer identity) |
| value/inline classes, callable refs, `by` delegation, multi-module, expect/actual | ~rest | each its own language feature |

**Conclusion (authoritative):** the 69-corpus needs a STACK of features together — Json/companion
resolution + generics + custom/contextual serializers + sealed + polymorphic + reflection +
descriptor introspection + several parser gaps. **Every file has ≥1 blocker from this stack, and the
simple ones have several** — so no single bounded fix moves 0/69 → 1/69. This is a multi-session
language roadmap, not an extension-surface gap. (Raw per-file data: `/tmp/ser_errs.txt` at audit time;
reproduce by compiling each `boxIr/*.kt` with the serialization runtime on `-classpath`.)

#### Next bricks (highest leverage first, by the audit)
- **Classpath companion-object resolution (`Json` = gap #3, 14 files):** resolve a bare classpath
  class name to its companion instance (`getstatic C.Default`/`.Companion`) + members. General Kotlin
  feature, not serialization-specific. Won't make a file fully pass alone (secondary blockers) but is
  the single biggest first-blocker.
- **Default values (`val a: Int = 5`):** the `seen` bitmask — `addElement(name, isOptional=true)`, the
  synthesized `C(int seen, …fields, SerializationConstructorMarker)` constructor, decode-from-mask.
- Then enum/collection (`ListSerializer`, `EnumSerializer`)/generic/sealed serialization, then the
  language features for the rest of the 69-corpus.

The plugin's critical path is **gap #7 alone**: the plugin must build correct IR for the `$serializer`
(an `object` implementing the generic `KSerializer<Foo>` interface — `descriptor` field initialized in
the object's `<init>`/`<clinit>` via `NewExternal(PluginGeneratedSerialDescriptor)` + `addElement`
calls, `serialize` via `invokeinterface` `beginStructure`/`encode*Element`/`endStructure`,
`deserialize` via a `decodeElementIndex` loop, `childSerializers` of builtin serializer singletons,
**plus the erased generic bridges** `serialize(Encoder, Object)` / `deserialize(Decoder): Object`),
and krusty's emitter must accept all of it. Then a **real-kotlinc-compiled `box()` driver** does the
Json round-trip against krusty's classes (eliminating the Json cluster #3). This is one focused
codegen track — substantial, but a single track, and the emit primitives (objects, interface calls,
`while`/`when`, `NewExternal`, bridges) largely exist. The next session should target gap #7 directly.

Plus: real `serialize`/`deserialize` bytecode bodies and wiring the plugin into the emit path. And the
full 69 `testData/boxIr` corpus additionally needs sealed/polymorphic/generic/inline-class/contextual
support outside krusty's IR subset (gate 1303/7351). This is multi-day **core-compiler** work, not
surface work. `tests/serialization_conformance.rs` encodes the end-state as an `#[ignore]`d round-trip
test (executable spec) plus a guard test that flips when the blockers close.

This document and the tests state these boundaries explicitly rather than implying the bar is met.
