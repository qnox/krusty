# krusty architecture — multiplatform backends

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
- **Lowering split:** common `fir_lower` may desugar Kotlin semantics only. Target/runtime-dependent
  rewrites (JVM callable-reference classes, captured-var `Ref$*Ref` holders, counted-loop range
  optimizations, primitive/boxed ABI choices, value-class erasure, suspend CPS) belong in named
  backend lowering passes. If a temporary common-lowering hook is needed while the IR lacks a neutral
  node, keep the hook narrow, backend-owned, and record it as migration debt rather than adding JVM
  spelling or platform policy to core lowering.
- **Library ingestion is split the same way.** A compiled Kotlin library's *container* is
  target-independent and lives in core: `src/klib.rs` reads a `.klib` in either shape the toolchain
  ships (a zip for JS/wasm, an unpacked directory for Native), parses its `default/manifest`, and
  reports the `default/linkdata/package_*` metadata fragments and `default/ir/*` entries. It depends
  on no other compiler module — an architecture guard enforces that — because Native, JS and wasm all
  read the same container and only differ in what they do with a decoded declaration. Turning a
  fragment into symbols is a *symbol source* concern; turning a symbol into a representation is the
  backend's. The JVM backend consumes the core reader too, for the common `expect` headers Kotlin
  ships in a klib beside `kotlin-stdlib.jar`, which is a use of a klib and not a second reader.
- **Process front ends** are separate workspace packages. The root `krusty` package is a compiler
  library and exposes frontend and backend contracts. `crates/krusty-cli` owns kotlinc-compatible
  batch argument parsing, filesystem output, and process exit behavior, while `crates/krusty-lsp`
  owns its in-memory source-set analysis, JSON-RPC, document lifecycle, and compact editor query
  snapshots. These packages depend toward the compiler library; the compiler never depends on any
  process adapter. LSP compiler analysis is an internal module—not a single-consumer workspace
  package—and architecture guards keep it isolated from the long-lived protocol/session modules.
- **Shared process-independent policy** stays in the compiler library when it is genuinely a compiler
  concern. For example, JVM classpath code resolves a JDK home to `lib/modules` (or a pre-jimage
  JDK's `jre/lib/rt.jar`); each executable independently decides how its own arguments select that
  home. There is no command-layer “common” crate until both executable packages share a stable
  command abstraction rather than a few flags.

## Build-layer boundary

`crates/krusty-build` owns process-independent build orchestration. It models compilation units and
their dependency DAG, constructs content-addressed cache keys, stores complete artifact sets, and
drives a compiler through an explicit environment boundary. The crate is a consumer of the compiler
library; neither the compiler nor a target backend may depend on it.

The build model is wider than an analysis-only project model. A build unit records resources, Java
sources, module name, processor inputs, per-module JDK selection, friend paths, and output shape
because silently ignoring any of those can cache a short or semantically different artifact. An
environment that cannot honor a recorded input must refuse the unit before computing a cache key.

`crates/krusty-toolchain` reads Kotlin Toolchain projects (`project.yaml` and `module.yaml`, the
format JetBrains' `kotlin` command builds) and is meant to build them with krusty, the way krusty
stands in for `kotlinc`. It reports the toolchain's own diagnostics, checked against output recorded
from the real `kotlin` command, and refuses by name whatever it does not implement. See
[KOTLIN_TOOLCHAIN.md](KOTLIN_TOOLCHAIN.md).

Gradle projects are compiled by running that project's `./gradlew`, not by reimplementing the
Kotlin plugin. `tools/krusty-gradle` is a Gradle plugin (`id("krusty")`, sources under
`src/main/kotlin`). A build applies both its selected `org.jetbrains.kotlin.jvm` 2.4.x plugin and
`krusty`; source sets, `kotlin {}`, compiler options, and `KotlinJvmCompile` stay. Source checkouts
can use `--include-build`; releases include a portable local Maven repository with the implementation
and plugin-marker publications. The Kotlin JVM plugin still owns source sets, `project()` edges,
compiler options, classpaths, friend paths,
and compile outputs. The public Kotlin task is disabled and depends on a typed, cacheable replacement
task whose `ExecOperations` and `FileSystemOperations` are injected by Gradle. The replacement execs
the krusty binary (`-Pkrusty.binary` or `KRUSTY_BIN`) with the task's complete `.kt` and same-module
`.java` sources and normalized structured compiler options. kotlinc and the Kotlin compile daemon
are not started. The replacement wiring runs at the end of project configuration: realizing a
`KotlinJvmCompile` during plugin application reads and finalizes
`KotlinTopLevelExtension.compilerVersion` before convention plugins set it, and Gradle forbids
registering a task from inside a task configuration callback. The plugin does not invoke krusty-build, inspect task implementation classes, or
mutate private action lists. `.kts` files are not compilation inputs.

Two kinds of `KotlinJvmCompile` stay with kotlinc, and the plugin logs each one at configuration
(`krusty: task '…' is left to kotlinc: …`). A task with no `sourceSetName` belongs to no source-set
compilation: from Gradle 8, `kotlin-dsl` registers `compilePluginsBlocks` as one; it compiles the `plugins {}`
blocks extracted from precompiled script plugins as scripts, with no Java sibling or classes output.
Every compile of a project applying `kotlin-dsl` (`org.gradle.kotlin.kotlin-dsl.base`, so `buildSrc`
and convention-plugin builds) also stays: its precompiled script plugins are `.gradle.kts` scripts,
its SAM-with-receiver and assignment compiler plugins change call resolution, and its compiler
settings add arguments the adapter does not model. Replacing such a task would drop the scripts or
reject the build, so neither is replaced and neither joins `krustyCompile`.
(`kotlin_dsl_build_src_compiles_through_krusty` in `crates/krusty-build/src/gradle.rs`.)

The adapter derives Kotlin semantics from `KotlinBasePlugin.pluginVersion`. It reads the experimental
Build Tools API `compilerVersion` property behind the two exact KGP/BTA opt-ins only to reject an
override which would make those semantics diverge; it does not use the experimental compiler path.
A pre-release, RC, or dev suffix is a different version and is rejected. `allWarningsAsErrors` is
forwarded as `-Werror`; an explicit `-Werror` alongside it is the same idempotent policy and is
forwarded exactly once.

Gradle owns incremental invalidation. An unchanged replacement task is UP-TO-DATE and does not run
krusty. Once any source, classpath, friend path, plugin, compiler option, or compiler binary changes,
the task cleans its Kotlin output directory and recompiles the complete source set. That deliberately
coarse boundary keeps source additions and removals, multifile facades, generated classes, and
`.kotlin_module` metadata correct without reconstructing Kotlin's dependency graph from class files.
Compiler plugins pass through the adapter as kotlinc receives them: each compile task's KGP
`pluginClasspath` becomes `-Xplugin=<jars>` and its `pluginOptions` become
`-P plugin:<id>:<key>=<value>`, and the compiler resolves each jar by the registrar it declares.
Plugin CLI free arguments stay rejected in favour of that structured input. The applied public
`KotlinCompilerPluginSupportPlugin` implementations, collected whether they were applied before or
after krusty, gate the task before krusty runs: `org.jetbrains.kotlinx.serialization` runs as
krusty's native pass, and `kotlin.scripting` — kotlinc's default scripting plugin, which KGP applies
to every project and puts on every plugin classpath — acts only on script sources, which the adapter
never passes. Any other applied support plugin fails the task, and a plugin jar added to the plugin
classpath without one is refused by the compiler.

Build correctness rests on these contracts:

- A dependency graph edge is a relation. Providers may repeat an edge, but the graph canonicalizes
  it once, preserving first-seen order. A duplicate must never become an extra indegree and a false
  cycle.
- A module base directory is absolute, and every relative module path is resolved against it when
  the module enters the graph. Output roots have unique, non-overlapping owners, may not be a
  filesystem root, and may not overlap declared source, resource, classpath, processor, or JDK
  inputs. A friend path owned by another module requires that owner as a dependency, so the friend
  output is materialized before compilation.
- A cache key covers the compiler binary, target and scalar flags, ordered source paths and bytes,
  every explicit and implicit classpath/toolchain input, friend inputs, plugins, and direct
  dependency ABI identities. Environment values are keyed as raw OS bytes with presence preserved.
  Opaque argument files or path-bearing flags are rejected until their referenced bytes have an
  owned representation.
- A cached module is the complete ordered artifact set plus the identity published to dependents.
  Materialization replaces old output trees, including every declared output; it never overlays a
  subset or silently chooses the first output. A concrete environment that emits only one artifact
  tree refuses a module declaring multiple independent outputs rather than copying that tree.
- A store manifest is complete only with an exact record count, terminal marker, and integrity
  digest covering the published ABI and every artifact record. A valid prefix or ABI-only mutation
  is a miss, never a hit.
- A reduced ABI identity may be published only when it includes every fact a dependent can observe:
  Kotlin metadata, non-source annotations, constants, contracts, sealed and enum structure,
  `.kotlin_module`, and inline bodies and their reachable synthetics. Until that proof exists for an
  environment, it publishes a whole-output identity. Rebuilding too much is acceptable; a stale
  dependent cache hit is not.
- Backend-specific adapters own representation inputs. The JVM adapter, for example, records the
  exact JDK image and any companion metadata input discovered from its classpath, and passes friend
  paths through the compiler's friend-path contract rather than treating friendship as ordinary
  visibility.

## Language-server memory model

- `serde`, `serde_json`, JSON-RPC transport, and session state belong to the separate
  `crates/krusty-lsp` workspace package. The compiler's dependency graph has no server dependency
  or server-specific feature. Within the LSP package, `compiler_analysis` is the only module allowed
  to inspect checked frontend data; protocol/session modules consume compact snapshot contracts.
- The supervisor accepts kotlin-lsp's stdio launch subset, not its general CLI. `--stdio` is
  optional: with no transport flag the server still speaks stdio, while official kotlin-lsp
  defaults to a socket. `--version`, socket listen/connect, `--client`, and `--multi-client` are
  unsupported; a launch that selects socket mode without `--stdio` is rejected so the process does
  not claim a port it is not listening on. `--system-path` is that system's directory, and Krusty's
  dependency cache is the owned child `<path>/krusty/deps` (startup collection never treats the
  system directory itself as the cache root). `--log-level` and `--log-category`, and the
  `KOTLIN_LSP_LOG_LEVEL` / `KOTLIN_LSP_LOG_CATEGORIES` defaults, are validated and then discarded:
  the server has no logging owner that applies them.
- The LSP supervisor never runs the compiler in its own long-lived process. It sends source sets to
  a compiler worker that is restarted after 64 analyses, or sooner when a Linux `VmRSS` sample
  shows that process above the supervisor's ceiling (default 1 GiB). The sample is taken before a
  request. It is not a host or cgroup budget: a tighter limit can still OOM-kill the child, and one
  request can grow past the ceiling before the next sample. Those deaths use the existing crash
  restart. An unreadable sample, including every non-Linux process, leaves the worker in place.
  The ceiling bounds retained growth from the compiler's process-lifetime name/type interners
  between requests, while JVM classpath initialization stays amortized across edits.
  Replacing that process is one transition: kill, poll, and join its stdout reader share a single
  deadline. A child that is still running, or a wait that fails for a reason other than the process
  no longer being a child, stays in a one-slot park and blocks the next spawn.
  The request also carries the bounded set of enabled language-feature names derived from project
  compilation arguments and explicit LSP flags; per-source directives are applied inside the worker.
  The worker is not a second server-CLI consumer: `exec` carries only its private mode marker and
  supervisor PID. Before compiler initialization, the supervisor sends one bounded launch frame with
  the already-composed project/JDK classpath; restarts use the same frame. This keeps arbitrarily many
  individual classpath entries out of platform argument/environment size limits and gives initial
  startup and project reconfiguration one classpath-composition rule.
- Diagnostics use either pull responses with refresh requests or
  `textDocument/publishDiagnostics`, according to the client's capabilities. Compiler diagnostics
  are deduplicated by `(span, severity, kind, message)` before entering the LSP indexes.
- Closed workspace files are indexed only after interactive analysis has been served. One cached
  project-model inventory feeds a module-neighbourhood priority and a lower-priority full sweep;
  watched source changes requeue their own file without rebuilding that inventory. The inventory
  walk is unlimited in tree depth and entry count and reports its progress — start, throttled
  discovered-source checkpoints, and a finished summary with file count and elapsed time — through
  the same work-done status token and the client log. Queue count and
  owned-URI bytes are bounded, and a project-model replacement advances a generation, discards
  queued work, and clears old retained results before replacement analysis can arrive. Each
  background chunk goes through the same module visibility, language-feature, and classpath
  selection as open documents, but uses separate source-discovery and analysis caches so indexing
  cannot evict the interactive hot path. One work-done token spans the queued operation; every
  chunk updates an admitted-files `(handed out, total)` pair, and priority promotion changes queue
  ownership without double-counting the file.
- `workspace/symbol` composes project declarations with a final dependency-class layer built from
  the model's compile classpath after declared project outputs are removed. All layers share one
  parsed query grammar, input ceiling, wildcard transition budget, keyboard-layout fallback, rank
  ladder, response count, and response byte budget; a storage layer cannot reinterpret or expand
  the request. The dependency index retains only interned class/package names. It ranks at most the
  response slots left by project declarations, then asks the compiler worker to materialize only
  those survivors. Materialized text is content-addressed on disk, reduced to path and precomputed
  UTF-16 declaration endpoints when its engine event reaches the session, and then dropped; the
  session does not retain a second source copy.
  In-flight materializations share that same entry ceiling, bounding the engine queue while leaving
  unadmitted candidates eligible for a later query.
  Indexes, in-flight requests, and completed locations carry the project-model generation, so an
  old classpath cannot repopulate the session after reset. A completed failed materialization
  releases its in-flight marker and may be retried; failure never becomes a permanent negative
  cache. Raw per-jar class listings are a best-effort startup cache keyed by path, size, and mtime;
  malformed or unavailable cache state always falls back to reading the classpath entry. They are
  auxiliary entries under the same version root and global lock as rendered sources, so the same
  age/size collector and both cache-clean modes cover them rather than leaving an unbounded side
  directory.
- Workspace diagnostics retain only bounded file URIs, text hashes, packed UTF-16 ranges/severity,
  and deduplicated display messages. Replaced entry slices are compacted and deleted file slots are
  reused; no source text, AST, semantic class identity, classpath entry, or compiler snapshot
  survives the chunk merge. Workspace-wide pull responses honor prior result ids and emit
  tombstones for disappeared files, but are cancelled with a non-retriggering protocol error when the
  bounded non-streaming report would exceed its item, byte, or message limit. Each accepted changed
  chunk publishes diagnostics for its closed attempted files only when the client does not support
  pull diagnostics; open documents are always excluded because their buffers supersede disk snapshots.
- An open document retains its source text, a bounded compact diagnostic cache for published and
  pull diagnostics, and compact indexes for hover, completion, definitions, document symbols,
  signature help, folding ranges, and semantic highlighting. The compiler's full diagnostic vector
  is short-lived; only packed locations/severity and interned messages cross into session state.
  Folding ranges are 28-byte packed records containing precomputed UTF-16 locations, kind/style
  tags, and an optional summary byte span into the authoritative open document. Collapsed labels
  are reconstructed only while encoding the bounded response; neither the snapshot nor an AST node
  retains copied source text. Each hover
  entry is a 12-byte `(source lo, source hi, declaration-signature id)` record; official-format
  signature strings are deduplicated per document and bounded across the source set. No AST node or
  hover entry retains a source-text copy. A scoped completion
  entry is a 24-byte packed array of scope bounds, declaration position, interned label/label-details
  IDs, item kind, and optional receiver type; member entries are 16 bytes. Completion scopes these
  cached records to the cursor position and receiver — including parser-recovered
  `receiver.`/`receiver?.` expressions — but deliberately does *not* filter by the typed identifier
  prefix, so the response carries the full, prefix-independent candidate set and the editor filters
  it locally as the user types. This is done without retaining the AST or invoking the worker;
  source-item resolution returns the already complete item unchanged.
  A document retains member catalogs only for
  receiver types referenced by its own lexical symbols/source, rather than duplicating every member
  in the open source set. A shared source-set budget caps completion at 32,768 records and a
  conservative 4 MiB wire estimate. The response is client-filterable — `isIncomplete: false` —
  only when the snapshot is trustworthy: analysis is current (no pending edit or in-flight
  re-analysis) and the budget dropped no candidate. Otherwise the snapshot is a strict subset of
  the visible symbols and the response keeps the refinable `isIncomplete: true` contract so the
  editor re-queries as the snapshot catches up. This deliberately differs from
  the official Kotlin LSP, which refines server-side and always returns
  `isIncomplete: true`. Each
  semantic token is a 16-byte `(UTF-16 line, start, length, type, modifiers)` record, positioned once
  in the compiler worker so full/range requests neither rerun analysis nor rescan source. Worker JSON
  uses packed array entries rather than repeating object keys, and range encoding binary-searches
  the sorted snapshot before allocating its result. A definition entry is a 20-byte
  `(source lo, source hi, target file, target lo, target hi)` array with no retained strings; a shared
  256K-entry budget bounds definition, type-definition, and implementation construction and
  long-lived storage.
  The short-lived compiler catalog derives transitive source subclasses and exact member overrides,
  using checked-signature and arity indexes instead of cross-scanning overload sets. A constant-factor
  budget bounds hierarchy traversal, substitutions, and structural type comparisons. Compact source
  declaration references and parser-owned generic-supertype `TypeRef`s carry substitutions across
  non-declaring intermediates; only proven declaration edges are then closed transitively. Temporary
  type patterns retain no source text, and the catalog is dropped before the supervisor receives the
  packed entries. Find-references deduplicates
  cursor targets into a request-local set, reverse-scans each bounded entry once, and allocates only
  the returned locations rather than retaining a duplicate reverse index.
  Rename uses the same integer-only definition entries. During response encoding it reads identifier
  spellings from the one authoritative open-document string, computes bounded minimal text changes,
  and resolves every edit endpoint to UTF-16 in one forward source pass per affected document.
  Request-local spelling and replacement buffers are capped and dropped with the response; no AST,
  compiler snapshot, or document state retains another source string.
  Type-definition and implementation use the same 20-byte representation and consume the remainder
  of that shared 256K-entry navigation budget after definition entries are built. Splitting a
  saturated budget among the three indexes cannot enlarge the prior worst-case navigation worker
  frame.
  The compiler worker reduces checked explicit, inferred, nullable, constructor, ordinary-property
  declarations, and property-result types directly to source class spans, then drops type tables,
  ASTs, and its short-lived type-target map. The supervisor retains no class names or copied source
  for these queries.
  A document-symbol entry is a 40-byte packed array containing an interned name id, precomputed
  UTF-16 full/selection endpoints, kind/deprecation bits, and a parent index. The worker flattens
  compiler declarations into this hierarchy, caps the source set at 32,768 entries and a conservative
  8 MiB response estimate, then drops the AST and source-derived temporary spans. Requests rebuild
  JSON from the packed hierarchy without retaining or recreating a second source string.
- Open documents are analyzed as one source set, so one parse/signature pass resolves declarations
  across open files and refreshes every open file's diagnostics, completion, hover, document symbols,
  signature help, and highlighting snapshots atomically. Temporary source-set catalogs carry
  completion declarations and source-only highlighting flags such as `data`, `operator`, and
  `Deprecated` across files while the compact snapshots are built. Navigation also consumes
  checker-selected source declaration ids for overloads before reducing definitions and transitive
  implementations to file/span pairs. AST, symbol-table, full type-analysis, and those catalogs
  are dropped after each analysis; closing a document removes its source and compact query indexes.
- Input frames are capped at 16 MiB, headers at 8 KiB, and the reader-to-dispatch queue at four
  parsed messages. Open text is capped at 32 MiB across at most 256 documents; worker JSON encoding
  is capped at 64 MiB in both directions. Document-state bursts are capped by count, retained bytes,
  and elapsed time; ordered changes are applied in one analysis. A later single full-document
  replacement can supersede an earlier queued change, but incremental changes are never discarded.
  Coalescing never crosses another document notification. Each notification is capped at 256 edits
  and cumulative UTF-16 conversion and text-mutation work are each capped at three source-set
  passes. Replaced text retained for rollback is capped at one source set. A worker analysis is
  terminated and restarted after 30 seconds.
- The server advertises incremental synchronization and translates LSP UTF-16 ranges directly into
  byte ranges in the one retained document `String`. Accepted ranged edits mutate that allocation in
  place. A request-local undo log keeps only replaced fragments so an invalid multi-edit notification
  can restore the original version without retaining a second source copy or adding a rope/piece-table.
  Stale versions do not trigger analysis. If a document was cleared after exceeding the aggregate
  source-set limit, ranged edits are rejected without advancing its version until a full replacement
  restores synchronization.

## Invariants

- **No non-backend module depends on a backend.** `resolve.rs`/`types.rs` must not reference
  `jvm::`. (Helpers that traffic in JVM `ClassInfo`/descriptors belong in the backend.)
- **Semantic behavior is independent of symbol origin and source spelling.** Once declarations have
  entered the symbol model, resolution, checking, and lowering must not select a different algorithm
  because a declaration came from the current file, another module, Java source, a classpath class,
  Kotlin metadata, or a generated source. Loaders and decoders normalize missing facts at their
  boundary; downstream code consumes the common facts. Selected callables carry declaration
  capabilities such as interface dispatch regardless of provider, even when one provider could be
  queried again later. Likewise, a package, module, file, class, or host path must not act as a routing
  key. A language or JVM rule that genuinely names a declaration is represented once in the backend's
  documented semantic mapping, rather than by scattered conditionals at use sites.
- **A call safety decision consumes the checker-selected target.** `TypeInfo::resolved_calls` carries
  suspend and inline capability on every callable target, including same-module extensions and class-
  body extensions. Lowering gates query that exact `ResolvedCall`; they must not scan declarations by
  the callee's text, reconstruct overload selection, or maintain separate file/module/classpath tests.
  This is both a consistency rule (the checked overload is the emitted overload) and a privacy rule
  (a rejection reason need not expose the selected declaration's real name).
- **Kotlin property access remains a property through common IR.** A provider may discover a Java
  field or another platform storage declaration, but it normalizes that declaration into the same
  semantic property candidate shape used by Kotlin accessors. Checked FIR and common IR retain only
  the selected opaque `ExternalPropertyId`, receiver placement, parameters, and semantic result;
  they never classify the access as a field read or invent an accessor call. The target backend owns
  the provider handle and alone chooses the physical field/accessor operation. Providers still expose
  inaccessible declarations for hiding rules, and inheritance is traversed exactly once by the
  frontend over normalized declarations rather than repeated by each provider.
- **Generated classfile overlays preserve declaration structure, not encoded-name guesses.** A Java
  source stub records the parser's syntactic enclosing declaration and simple name, then emits the
  member type's exact `InnerClasses` self entry. Visibility and inherited-classifier lookup consume
  that structural entry; they never split `$` out of a JVM name, because `$` is also legal inside a
  Java identifier. One semantic access-flag derivation feeds both the class header and self entry so
  interface, annotation, enum, record, and class modifiers cannot drift between the two views.
- **Unsupported input has one explicit semantic boundary.** A not-yet-implemented language shape is
  rejected with a stable reason before emission; it is not silently redirected to a weaker lookup,
  dropped only for one declaration origin, or allowed to reach unverifiable bytecode. Returning an
  `Option` or `Result` is not itself a violation: the caller must preserve the declared rejection
  contract instead of inventing a fallback. Tests assert both successful behavior and intentional
  rejection boundaries against general input shapes, never project-specific names or paths.
- **No hardcoded type/alias tables.** Stdlib types resolve from the classpath; the Kotlin↔platform
  mapping is the ported `JavaToKotlinClassMap` (`jvm/jvm_class_map.rs`) — a *JVM-backend* table. WASM
  and JS backends carry their own mapping.
- Representation (primitive vs boxed, value-class unboxing, …) is a **backend** concern, never the
  checker's.

## Current coupling to remove (the migration)

The front end is not yet fully decoupled. The concrete blockers, in priority order:

1. **`types::Ty` still conflates Kotlin semantic identity with target/runtime shape.** JVM descriptor
   formatting has moved out of `Ty`, but `Ty::Obj(TypeName, …)` still stores names that are sometimes
   Kotlin builtins and sometimes JVM/internal runtime classes (`java/lang/String`, `java/lang/Object`).
   Function-type supertypes now retain semantic `kotlin/FunctionN`; their
   `kotlin/jvm/functions/FunctionN` carrier is owned exclusively by the JVM backend.
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
