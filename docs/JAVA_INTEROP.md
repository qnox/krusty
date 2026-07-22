# Java-source interop & annotation processing (APT/KSP) — analysis and plan

Scope: close the conformance-harness gaps around `// FILE: *.java` box tests, and define how
mixed Java/Kotlin compilation plus multi-round annotation processing (APT and KSP) fit krusty
with minimal compile-time cost. Slice 1 (javac-first harness interop) is landed; later slices are
design here first.

## 1. The corpus prize (measured, 2.4.0 box corpus, 7351 files)

| shape | count | status |
|---|---|---|
| single-module tests with inline `// FILE: *.java` | 5 | 1 excluded (`IGNORE_BACKEND_K2`), 2 PASS via slice 1, 1 skip (SAM-ambiguity `LANGUAGE` flag), 1 skip (Java→Kotlin reference, needs slice 2) |
| `// MODULE:` tests with `.java` files | 0 | `split_modules` keeps declining them — nothing to win |
| standalone `.java` files on disk | 0 | corpus embeds Java only via `// FILE:` blocks |

The box gate's Java prize is small; the real payoff of the pipeline below is the **drop-in
mixed-build story** (Gradle modules with `src/main/java`) and hosting APT — both need the same
machinery, so the harness is the cheap validation vehicle.

## 2. Compilation-order design space

Mixed Kotlin/Java has three reference-orderings:

- **A. javac-first** (landed, slice 1): javac compiles the `.java` sources against the directive
  classpath; the output dir joins krusty's classpath (loose-`.class` dirs are already first-class
  `Entry::Dir` classpath entries); krusty compiles Kotlin normally. Covers Kotlin→Java references
  only. Java→Kotlin references make javac fail → the harness **skips, never mis-grades**.
- **B. Kotlin-first with Java source stubs**: krusty reads Java *sources* for signatures (a
  header-only Java parser: package/imports/type decls/member signatures — no bodies), compiles
  Kotlin against those stub symbols, then javac compiles the Java with krusty's output on the
  classpath. Covers Java→Kotlin references (`kt40180_3.kt`: `class B<E> extends A<E>` where `A` is
  Kotlin). This is kotlinc's model (its frontend indexes Java sources; javac runs after).
- **C. Joint fixpoint**: only needed when annotation processing generates sources for both
  languages — see §4.

Slice order: A (done) → B (a bounded Java *signature* parser; bodies never parsed) → C only as the
APT/KSP host demands it.

## 3. Minimal-performance-penalty rules (what slice 1 enforces)

- **Zero cost when no Java present.** The `.java` split happens inside the existing `// FILE:`
  block scan; a test without Java blocks allocates one extra empty Vec, nothing else.
- **No process spawns.** javac runs **in-process** in the already-persistent `JavaRunner` JVM
  (`ToolProvider.getSystemJavaCompiler()`), reused by the Java-driver e2e suites. Protocol change:
  the `driver` field carries N path-separator-joined `.java` paths; an empty `mainClass` means
  compile-only. Warm cost per tiny compile: ~10–30 ms vs ~300–500 ms for a cold `javac` spawn.
- **No new daemons.** The JavaRunner pool (`server_pool_cap`) already exists; the corpus has ≤5
  Java tests per run, so contention is nil.
- **Classpath reuse.** The javac output dir is a normal `Entry::Dir`; per-entry L2 caches make the
  Kotlin compile's reads of those classes as cheap as any dir-module dependency.

Measured slice-1 effect on the box gate: +2 PASS (kt40180, kt40180_2), 0 new failures, penalty
confined to the 4 eligible tests.

## 4. APT / KSP in a multi-round environment

Definitions: **APT** = javax.annotation.processing inside javac (Dagger, Micronaut, Room's Java
half). **KSP** = Kotlin Symbol Processing (codegen-only; reads a resolved symbol view, emits new
files). Both are *codegen-only fixpoints* — the model `src/plugins/ksp.rs` already implements and
`tests/ksp_real_e2e.rs` proves against a real from-jar `SymbolProcessor` **including multi-round**
(a round-1-generated annotated file is re-fed and processed in round 2, and the round-2 output
compiles to bytecode).

### What exists

- KSP host fixpoint driver with max-round backstop (`KspHost`), duplicate-suppression across
  rounds, and the sidecar architecture written up in `docs/PLUGIN_API.md` (shim JAR implements
  `Resolver`/`KSClassDeclaration` over IPC; the processor JAR runs unmodified).
- Real-KSP e2e (opt-in `KRUSTY_KSP_E2E=1`): 14-capability matrix + multi-round.
- In-process javac seam (slice 1) — the same seam APT rides on, because **APT rounds are native to
  javac**: passing `-processor`/`-processorpath` to the embedded compiler gets javac's own
  multi-round loop (generate → new round → fixpoint) for free. We do not reimplement APT rounds.

### The combined multi-round loop (slice C design)

One outer fixpoint owned by krusty's driver; each iteration runs at most two inner engines that
already have their own fixpoints:

```
loop:
  1. resolve Kotlin + Java-stub symbols (B)              — krusty
  2. KSP round(s) over that view → generated .kt/.java   — sidecar (own fixpoint)
  3. javac -proc:full over all .java (incl. generated)   — JavaRunner JVM; APT rounds internal
     → .class dir + APT-generated .java handled inside javac's own loop
  4. any NEW .kt from (2)? → go to 1, else done
emit: krusty compiles final Kotlin set against final javac output dir
```

Termination: KSP's backstop bounds (2); javac bounds (3) internally; the outer loop only repeats
when a round adds a new *Kotlin* source, and generated files are deduped by path+content, so the
outer loop is bounded by the KSP backstop too.

### Multi-round performance levers

- **One JVM for everything JVM-side.** javac+APT and the KSP sidecar can share the persistent
  JVM; processor classloaders are cached across rounds AND across modules (this is what makes
  Gradle's kapt slow and KSP2 fast — classloader reuse is the single biggest lever).
- **Incremental re-resolution, not re-compilation.** Step 1 re-runs only signature collection over
  the delta (generated files), reusing the `Classpath` per-entry caches; krusty's index-based
  AST makes appending files cheap.
- **Stub-only Java parsing (B)** keeps the outer loop free of javac until the final round when
  bodies actually need compiling: rounds 1..n-1 only need *signatures* of generated Java.
- **Skip-fast paths:** no processors configured → the whole §4 loop is dead code; no `.java` → 3
  is skipped; no generated Kotlin → single outer iteration.

## 5. Slice plan

1. **[landed] javac-first harness interop** — `common::javac_compile` (persistent in-process
   javac, compile-only protocol), `.java` blocks in `compile_multifile`, resolver fix: static-call
   receiver on a same/root-package classpath class now resolves through the import levels
   (`imported_type_internal` fallback), matching ctor/type positions.
2. **[landed] Java signature stubs (B)** — `src/jvm/java_stub.rs` parses the Java signature
   surface and emits stub `.class` files (descriptors + generic `Signature` attrs; dummy bodies —
   stubs are never JVM-loaded). The harness falls back to Kotlin-first when javac-first fails:
   stubs → krusty → real javac against krusty's output → ship javac's classes. Resolution is
   callback-based (Kotlin module names + classpath); unresolvable types abort → skip.
   Fixed along the way: `open fun` members were emitted `ACC_FINAL` when nothing in-module
   extended the class — unsound across compilation boundaries (javac rejected the override);
   `FunDecl.is_open` now flows to `ir.open_methods`. `kt40180_3.kt` remains skipped on an
   unrelated pre-existing limitation (`listIterator` same-name overloads: "multiple overloads with
   different erased signatures").
3. **[landed] `// MODULE:` Java files** — `split_modules` keeps `.java` files per module
   (`ModuleBlock.java_files`); the gate compiles them javac-first against the module classpath
   (base + dependency dirs), appends the javac dir for the module's Kotlin, and chains BOTH class
   sets through the module dir; javac failure falls back to the Kotlin-first stub pipeline.
   Java-only modules supported. Zero corpus tests today — model completeness for drop-in.
4. **[landed] APT host** — `common::javac_compile_proc` passes `-processorpath` (+ `-proc:full`,
   required on JDK >= 23, and `-s` for generated sources) through the persistent JavaRunner; javac
   discovers processors via ServiceLoader and owns the multi-round loop. Validated self-contained
   (`tests/apt_host_e2e.rs`): an in-test-built `AbstractProcessor` (dir-based service registration,
   no jar needed) generates across TWO rounds (`@Gen Src` → `@Gen2 SrcMid` → `SrcMidEnd`), krusty
   compiles Kotlin calling the round-2 class, and a Filer collision surfaces as a failed compile
   (skip, never mis-grade).
5. **Combined outer fixpoint (C)** — wire KSP host + APT javac into the §4 loop behind the plugin
   registry; e2e: KSP generates an annotated Java file → APT generates a Java class → Kotlin
   references it.
