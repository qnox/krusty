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

Remaining for *full* conformance: 2-slot types (Long/Double), nullable/nested/richer types + real
`childSerializers`/`decodeSerializableElement`, wiring the plugin into the main compile path, and the
language features the 69-case `testData/boxIr` corpus needs. The flat round-trip proves the surface +
emitter are sufficient end-to-end; the rest is incremental codegen.

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
