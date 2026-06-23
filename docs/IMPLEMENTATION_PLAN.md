# krusty — implementation plan

Phased, each phase ends in a **green `cargo test`** and a runnable artifact. The pipeline is built
front-to-back so the streaming/arena shape is real from the start, then widened.

Legend: ✅ done · 🚧 in progress · ⬜ todo

## Phase 0 — Foundations  ✅
- ✅ Cargo project (lib + bin), local `cargo test`/`cargo run`. Toolchain: rustc 1.96 + gcc linker.
- ✅ `token.rs`: token kinds, `Span { lo:u32, hi:u32 }`, keyword table (types are idents, not kw).
- ✅ `lexer.rs`: byte-slice → `Vec<Token>`; idents, keywords, int/long/double/string/bool literals,
  multi-char operators, line+block comments, newline-as-token layout. 6 unit tests.
- ✅ `diag.rs`: `Diagnostic`, `DiagSink`, line/col rendering. 2 unit tests.
- ✅ **Exit met:** 8 tests green; driver lexes the real `multifile`/`bodyheavy` bench files
  (5254 tokens/file, 0 errors).

## Phase 1 — Parse to arena AST  ✅
- ✅ `ast.rs`: index-based arena (`ExprId/StmtId/DeclId` = `u32` into parallel `Vec`s; no Box/Rc
  graph, bulk-freeable). Decls (`fun`), stmts (`local/assign/return/while/expr`), exprs
  (literals/name/unary/binary/member/call/if/block). S-expr `debug_tree` for tests.
- ✅ `parser.rs`: recursive descent for decls/stmts; **Pratt** for expressions with the Kotlin
  precedence table (`|| < && < eq < cmp < add < mul < prefix < postfix`). Newline = terminator.
- ✅ Tests: 10 parser tests (precedence, assoc, paren, member-call, unary, if, block/while, package).
- ✅ **Exit met:** all `tests/cases/*.kt` + the in-subset bench files parse (multifile×20,
  many_functions = 500 decls). 18 tests green total.
- Note: `bodyheavy` uses `xor` (infix function) + `;` — **out of v0 subset**; not a krusty target.

## Phase 2 — Types & resolution  ✅
- ✅ `types.rs`: `Ty` (Int/Long/Double/Boolean/String/Unit/Error), numeric promotion, JVM
  descriptors, name↔type.
- ✅ `resolve.rs`: Stage C `collect_signatures` (global, cheap) + Stage D `check_file` (per-file
  typecheck): locals scope stack, name/call resolution, arithmetic+concat+comparison+logic typing,
  `if`-branch join, `val`-reassign error, return/while/assign checks, `println`/`toString`/`.length`
  intrinsics. Produces `TypeInfo { expr_types }` for codegen.
- ✅ 11 tests (arith/promotion, concat, comparison, if-join, return mismatch, unresolved,
  val-reassign, call arity/types, fib block, bool misuse).
- ✅ **Exit met:** driver runs lex→parse→collect→check; multifile (5000 decls) + many_functions
  (500) typecheck clean. 29 tests green.
- v0 decisions recorded: explicit return types required; exact-type assignment (no implicit widen);
  int literals = Int.

## Phase 3 — JVM class-file writer  ✅
- ✅ `codegen/classfile.rs`: `ConstPool` (Utf8/Integer/Long/Double/Class/String/NameAndType/
  Method+Fieldref, deduped, long/double 2-slot), `ClassWriter` (major 52 = JVM 8, matches kotlinc),
  method + `Code` attribute. `CodeBuilder` with **automatic max_stack/max_locals** tracking and the
  core opcode set (loads/stores, int/long/double const+arith+conv, returns, invoke*/getstatic).
- ✅ 5 unit tests (header/version, add builds, cp dedup, long 2-slot, stack tracking).
- ✅ **Exit met:** `tests/classfile_e2e.rs` emits `FooKt.add(II)I`; javac accepts it, `java
  -Xverify:all` verifies + runs it via a Java `Main` → `7`. Straight-line methods need no
  StackMapTable at v52; branch frames come in Phase 4.

## Phase 4 — Lower + emit the subset  🚧
### 4a — straight-line subset ✅
- ✅ `codegen/emit.rs`: direct AST→bytecode. Literals, numeric arithmetic (Int/Long/Double with
  widening), unary neg/not, free-function calls (`invokestatic` to the file class), `toString()`
  (→`String.valueOf`), string concat (→`StringBuilder`, the JVM-8 strategy; kotlinc uses
  `invokedynamic` — structural, not behavioral, difference), `println`, `.length`. Class naming
  `<File>Kt` + descriptors.
- ✅ **Exit met:** `tests/compile_e2e.rs` runs the full pipeline (parse→check→emit) on 8 functions;
  javac accepts, `java -Xverify:all` verifies + runs, all results semantically correct
  (`7,14,3,-5,8,11.0,42!,hi bob`). 38 tests green.
### 4b — `@kotlin.Metadata` emitter (protobuf)  🚧 (load-bearing for Kotlin-library ABI)
- ✅ `metadata/protobuf.rs`: protobuf wire writer, checked vs canonical vectors. 5 tests.
- ✅ `metadata/encoding.rs`: `bytesToStrings` (byte→char identity — **matches kotlinc 1.9.24's exact
  d1 payload** for `fun f(a:Int):Int=a`) + JVM modified-UTF-8; const pool now uses it. 5 tests.
- ✅ `writeData` layout known: `d1 = stringTable.serializeTo(out); message.writeTo(out)`; reference
  decoded as `mv=[1,9,0] k=2 xi=48 d2=[f,"",a]`.
- ⬜ **Remaining (the large part):** faithfully build `ProtoBuf.Package/Function/Type/ValueParameter`
  + `StringTableTypes` + the **qualified-name/builtins table** (so `kotlin/Int` etc. resolve) +
  JVM signature extension + the `@kotlin.Metadata` annotation attribute. This is effectively a
  re-implementation of `kotlinx-metadata-jvm`'s writer (~thousands of LOC) and is the single biggest
  remaining sub-project. Correctness gate = Phase 5b round-trip (kotlinc consumes krusty output).
  Note: a *Java* consumer needs none of this (it reads only the signatures, already matched in 5a);
  `@Metadata` is required only for *Kotlin* consumers.
### 4c — branches (if/while/comparisons/`&&`/`||`) ✅
- ✅ Label/branch support in `CodeBuilder` (if*/if_icmp*/goto/lcmp/dcmpg + offset linking).
- ✅ Emitter: comparisons (Int/Long/Double), short-circuit `&&`/`||` via `emit_cond_jump`, `!`,
  `if`-expression value + statement-`if`, `while`, block bodies, `val`/`var` locals + slots,
  `return`. Target lowered to **v50** so the type-inference verifier handles branches without
  StackMapTable (Java 8+ still loads v50; v52+frames is hardening, Phase 4e).
- ✅ **Exit met:** `control_flow_pipeline` e2e — `max/absdiff/both/either/classify/fib` compile,
  `java -Xverify:all` verifies + runs, all correct (`fib(10)=55`, `&&`/`||` short-circuit).
### 4d — streaming driver ✅
- ✅ `krusty [-d out] f.kt ...`: lex+parse all → global signatures → per file typecheck→emit→write
  `.class`→drop. Emits `ControlKt`/`ArithKt`; classes load + verify.
### 4e — v52 + StackMapTable ✅ (exact version match with kotlinc)
- ✅ All emitted methods now carry a valid `StackMapTable` attribute, required by Java 8
  (class-file v52). Branch targets tracked via `rec()` / `rec_s()` in `FunctionEmitter`;
  synthetic methods (`copy$default`, `equals`) register frames via `CodeBuilder.add_frame_if_new`.
- ✅ `init_temp` pattern: any slot added to `self.slots` via `alloc_temp` or `alloc_slot` before a
  `rec()` call gets a zero/null default store so the JVM's computed type matches the declared frame.
- ✅ Divergence-aware codegen: `goto`/store after a `return`/`throw` branch is elided; frames for
  dead code are filtered to avoid "bad offset" errors; duplicate-offset frames deduped.
- ✅ All `cargo test` green; `-Xverify:all` passes on all emitted class files.

## Phase 5 — Differential harness vs kotlinc  🚧
### 5a — ABI signatures + execution ✅
- ✅ Reference kotlinc: official 1.9.24 dist (run under JDK 21). `harness/run-diff.sh`.
- ✅ `tests/diff_kotlinc.rs` (env-gated `KRUSTY_KOTLINC`): compile same source with krusty + kotlinc;
  **public ABI signatures (javap) match exactly** and **execution output is identical** across an
  8-function subset (arith/promotion/mixed/if/&&/concat).
### 5b — @Metadata round-trip ✅ (Kotlin-consumer ABI ACHIEVED)
- ✅ The missing piece was the **`META-INF/<name>.kotlin_module`** file (maps package → file-facade
  class); `@Metadata` alone was already byte-exact. `metadata/module.rs` emits it (byte-exact vs
  kotlinc); driver writes `META-INF/main.kotlin_module`.
- ✅ **Round-trip passes** (`tests/metadata_roundtrip_e2e.rs`): krusty compiles a Kotlin library
  (`package demo`, `greet`/`addk`); the real kotlinc compiles a Kotlin **consumer** that imports
  them — resolves via krusty's `@Metadata` + `.kotlin_module` — and **runs** correctly (`hi bob`, `5`).
- ⇒ krusty output is consumable by both **Java** (signatures, 5a) and **Kotlin** (5b) consumers.
- Remaining for full @Metadata: classes/properties (richer proto), the JVM `method_signature`
  extension for non-derivable JVM names, multi-file facades.

## Phase 6 — Java interop + scale  🚧
### 6a — `.class` signature reader ✅
- ✅ `jvm/classreader.rs`: parses constant pool (modified-UTF-8), this/super, fields, methods →
  `ClassInfo`/`MethodSig` (name, descriptor, public/static). Round-trips krusty output; **validated
  against real javac output** (`tests/classreader_e2e.rs`: static/instance/private, primitive &
  reference descriptors, `<init>`). 2 unit + 1 e2e test.
### 6b — resolve Java static calls via the reader (dirs + jars) ✅
- ✅ `jvm/classpath.rs`: dir **and `.jar`** entries (zip/DEFLATE via `zip` crate), cached;
  `SymbolTable.classpath`; `import` capture; `resolve_java_static` (exact param-descriptor overload
  match) in typecheck + emit; driver `-cp a/classes:lib.jar`.
- ✅ **e2e**: krusty calls a javac class from a **loose dir** (`util.Calc`) *and from a real `.jar`*
  (`libx.Lib.sq` packaged with `jar cf`) → runs correctly (`15/[hi]/[12]`, `36`). 57 tests green.
- Remaining: JDK classes via jimage (classpath reader reads dirs/jars only), overload widening,
  multi-jar resolution, instance methods on arbitrary classpath types (needs `Ty::Obj`).
### 6c — minimal Java *source* front end ⬜ (signatures only, for mixed kt+java)
### 6d — scale benchmark ⬜ (peak RSS vs kotlinc on many_functions/multifile)
### 6e — `java.lang.String` instance methods ✅
- ✅ `resolve_string_instance` (curated `java.lang.String` subset: `length`/`isEmpty`/`substring`×2/
  `indexOf`/`concat`) drives typecheck + `invokevirtual` codegen. Interim until jimage gives the
  full JDK; each entry matches what kotlinc emits.
- ✅ **Differential pass**: `tests/diff_kotlinc.rs` now includes `s.substring(1)`, `s.substring(1,3)`,
  `s.indexOf("b")` — krusty's bytecode + execution match kotlinc exactly. Unit tests in `resolve.rs`.

## Phase 7 — Hardening  ⬜
- Fuzz the lexer/parser; property tests for arithmetic semantics vs a reference evaluator.
- Expand the subset opportunistically (when/nullable) only if it serves the memory thesis.

## Phase 8 — Classes (language surface)  🚧
### 8a — primary-constructor properties ✅ (Java-consumer ABI matches kotlinc)
- ✅ `class C(val a: T, var b: U)` → JVM class with **private backing fields** (`final` for `val`),
  a **primary constructor** (`super()` + field stores), and `getX`/`setX` accessors
  (`public final`). Property types restricted to the primitive/String `Ty` set in v0
  (class-typed members need `Ty::Obj` — a follow-up).
- ✅ Lexer `class` kw; parser primary-ctor params (require `val`/`var`) + optional empty body;
  AST `Decl::Class`/`ClassDecl`/`PropParam`; resolver registers `classes` (simple→internal name);
  `classfile.rs` field table + `getfield`/`putfield`; `emit::emit_class`; driver emits one `.class`
  per class and the `FileKt` facade only when the file has top-level functions.
- ✅ **Differential ABI passes** (`tests/diff_class_kotlinc.rs`): krusty + kotlinc produce **identical
  public member signatures** for `class Point(val x: Int, var y: String)` (ctor + getX/getY/setY),
  and both construct + run identically. Plus `tests/class_e2e.rs` (shape + `-Xverify:all` run).
### 8b — class `@Metadata` (kind=1) ✅ (Kotlin-consumer ABI for classes ACHIEVED)
- ✅ `metadata/class_builder.rs` emits `ProtoBuf.Class` (kind=1): fq_name (class-id via
  `DESC_TO_CLASS_ID`), supertype `kotlin/Any`, primary constructor (value params + JVM sig ext),
  and one property per field (name, return type, getter/setter JVM sigs; `var` adds flags=1798 +
  setter). Schema reverse-engineered + recorded in METADATA_NOTES.md.
- ✅ **Round-trip passes** (`tests/class_roundtrip_e2e.rs`): krusty compiles `class Point(val x, var y)`;
  the real kotlinc compiles a Kotlin consumer using **property syntax** (`p.x`, `p.y = ...`) — which
  only works if kotlinc reads the class `@Metadata` — and runs (`7:bye`).
- Note: d1 is semantically equivalent, not byte-identical, to kotlinc's (per-string string-table
  records vs kotlinc's range-compressed) — accepted by kotlinc, which is the ABI goal.
### 8c — member functions (instance methods) ✅
- ✅ Class bodies accept `fun` declarations → emitted as `public final` instance methods (`this` in
  slot 0, params from slot 1). Bare property names in a method body resolve to backing-field
  access (`getfield`/`putfield` for `var`). Typechecked with the class properties in an implicit
  `this` scope, parameters shadowing.
- ✅ Class `@Metadata` gains `Class.function` (f9) entries (name + return type + value params; JVM
  signature derivable, no ext — matching kotlinc).
- ✅ `tests/class_e2e.rs::member_function_shape_and_run` (instance method, `-Xverify:all`, → `15`)
  and the class round-trip now exercises a member call from a Kotlin consumer (`p.shifted(3)` →
  `7:bye:10`).
### 8d — reference types (`Ty::Obj`) ✅
- ✅ `Ty::Obj(&'static str)` (interned class internal-name; `Ty` stays `Copy`). `descriptor()` now
  returns `String` (`Lpkg/Name;` for classes). Two-pass `collect_signatures` builds a class universe
  first, so class types resolve regardless of declaration order / across files. `SymbolTable` carries
  `ClassSig` (internal name + ordered ctor properties + member-function signatures).
- ✅ Typecheck: class-typed params/locals/returns; **construction** `Point(args)`; **property read**
  `p.x`; **instance dispatch** `p.method(args)`; nested/chained (`l.to.translated(10).x`).
- ✅ Codegen: `new`+`dup`+`invokespecial <init>` for construction; `invokevirtual get<Prop>` for
  property reads; `invokevirtual` for instance calls; reference locals use `aload`/`astore`.
- ✅ Class `@Metadata` `Type.class_name` encodes `Obj` via a `DESC_TO_CLASS_ID` class-id (not Any).
- ✅ `tests/reftype_e2e.rs` (construct/access/dispatch across two classes, `-Xverify:all`, → `22`);
  `tests/reftype_roundtrip_e2e.rs` (real kotlinc consumes class-typed members via Kotlin syntax →
  `3:4:9`); resolver unit tests.
### 8e — `data class` ✅
- ✅ `data` soft keyword (still usable as an identifier). Synthesizes `componentN`, `copy`,
  `copy$default`, `toString` (`Name(p=v, …)`), `hashCode` (kotlinc's `result*31 + Type.hashCode`),
  `equals` (identity → `instanceof` → per-property compare). **Public ABI is identical to kotlinc**
  (`tests/data_class_e2e.rs` diffs `javap`); behavior matches under `-Xverify:all`.
- ✅ Class `@Metadata` sets `Class.flags = IS_DATA`; `componentN` carry the *operator* function flag
  and `copy` carries default-value param flags — so a Kotlin consumer compiled by the real kotlinc
  can **destructure** (`val (a, b) = p`) and **copy with named/omitted args** (`p.copy(y = 9)`).
  Verified end-to-end: consumer prints `Point(x=3, y=4)|true|Point(x=3, y=9)|3,4`.
- ⬜ **Next:** secondary constructors, inheritance/interfaces, nullability, generics, `when`,
  lambdas; facade `@Metadata` already encodes class-typed top-level function params.

## Phase 9 — kotlinc drop-in CLI  ✅
- ✅ `src/cli.rs`: kotlinc-compatible argument parsing — `-d`, `-classpath`/`-cp`/`-class-path`,
  `-module-name`, `-version`, `-help`, plus a table of accepted-but-ignored flags (with/without a
  value: `-include-runtime`, `-jvm-target`, `-no-stdlib`, `-language-version`, …). Unknown `-flags`
  are ignored with a note (never mistaken for sources). `@argfile`s expand inline.
- ✅ Sources may be `.kt` files **or directories** (scanned recursively); `.java` inputs noted as
  unsupported (no Java source front end yet).
- ✅ Output to a directory **or a `.jar`** (`-d foo.jar` → zip with `META-INF/MANIFEST.MF`, the
  `.class`es, and `META-INF/<module>.kotlin_module`).
- ✅ `tests/cli_dropin_e2e.rs`: the `krusty` binary compiles a source **directory** to a `.jar` with
  kotlinc-style flags; the real kotlinc compiles + runs a consumer against that jar (`8`). Plus
  `cli.rs` unit tests for flag parsing.

## Phase 10 — Kotlin conformance suite (ported)  ✅
- ✅ `tests/kotlin_box_conformance.rs` ports JetBrains/Kotlin's `compiler/testData/codegen/box`
  (10,009 `fun box(): String → "OK"` cases). Each is run through the real `krusty` binary; krusty
  **skips** what it can't compile (unsupported feature), **runs `box()`** on a JVM for what it can,
  and the test **fails only** if krusty *accepted* a case but produced wrong/invalid bytecode.
  Gated on `KRUSTY_KOTLIN_BOX_DIR`. Latest full sweep: **10,009 scanned · 13 compiled · 13 box()=OK
  · 0 FAIL** — krusty is correct on 100% of the conformance cases it accepts; coverage grows
  automatically as the language widens.
- ✅ `tests/box_vendored_e2e.rs` + `tests/box_data/` vendor the in-subset cases (Apache-2.0, see
  PROVENANCE.md) so they also run in normal `cargo test`.

## Phase 11 — `when`, control-flow & conformance hardening  ✅
- ✅ **`when`** expressions, both forms: subject (`when (n) { 0 -> …; 1, 2 -> …; else -> … }`,
  comma conditions, `==` match) and subjectless (`when { cond -> … }`). Lowered to an if-chain
  (subject stored once in a temp local); `->` is a real `Arrow` token; ABI matches kotlinc.
- ✅ **`if`/`when` branches may be statements** (`if (c) return x`) — wrapped as single-statement
  block branches. **`;`** is accepted as a statement/arm separator. **Reference `==`/`!=`**
  (String/class) lowers to `equals()`.
- ✅ **Conformance-driven fixes** (caught by the box harness, which asserts krusty never miscompiles
  a case it accepts):
  - exhaustive/diverging trailing `when`/`if` (all arms `return`) emits a dead default-return so the
    fall-through verifies (`when8.kt` → `OK`);
  - **string templates** (`"$x"`, `"${…}"`) and **raw strings** (`"""…"""`) are now *rejected* by the
    lexer (skipped, never silently miscompiled).
- ✅ Box conformance after this phase: **10,009 scanned · 26 compiled · 26 `box()`=OK · 0 FAIL**
  (up from 13); vendored set refreshed to the 26 in-subset cases.

## Phase 12 — `for` loops & compound assignment  ✅
- ✅ **`for (x in a..b)`** plus `a until b`, `a downTo b`, and `step s` over integer ranges, lowered
  to a counted while loop (start/end/step each evaluated once into locals; `DotDot`/`KwFor`/`KwIn`
  tokens). ABI matches kotlinc.
- ✅ **Compound assignment** `+=` `-=` `*=` `/=` `%=` (desugared to `x = x op e`).
- ✅ `parse_branch` generalized: an `if`/`when`/`for` body may be any single statement (e.g.
  `for (i in 1..n) s += i`), not just an expression.
- ✅ `tests/for_loop_e2e.rs` (runs on JVM, ABI vs kotlinc). Box conformance: 27 compiled / 27 OK /
  0 FAIL.

## Phase 13 — Nullable reference types  ✅
- ✅ Targeted via a data-driven scan of krusty's first-error across the box suite: `?` was the #1
  blocker (677 files). Implemented **`T?`** (nullable reference types; nullable *primitives* are
  rejected as out-of-subset), **`null`** literal, **`== null`/`!= null`** (→ `ifnull`/`ifnonnull`),
  **`!!`** not-null assertion (NPE throw; correctly distinguished from chained prefix `!`), and
  **`?:`** elvis. Reference `==` already lowered to `equals()`.
- ✅ Nullability shares the non-null JVM descriptor, so ABI matches kotlinc; krusty is permissive
  about null-safety (it never *miscompiles* an accepted program — the conformance invariant).
- ✅ `tests/nullable_e2e.rs` (runs on JVM incl. `!!`→NPE, ABI vs kotlinc). `?.` safe-calls are
  explicitly rejected for now (skipped, not miscompiled). Box conformance: 27 / 27 OK / 0 FAIL
  (nullable is foundational; it compounds once collections/`?.`/char literals land).

## Phase 14 — Modifiers, annotations & conformance fixes  ✅
- ✅ Data-driven (scanned the "expected a top-level declaration" bucket): **leading declaration
  modifiers** (`public`/`private`/`open`/`abstract`/`inline`/`operator`/`override`/`suspend`/
  `const`/… ) and **annotations** (`@Foo`, `@file:Bar(...)`) are now skipped before top-level decls,
  class-body members, and parameters. `@`, `[`, `]` are lexed. krusty treats everything as
  public/final (fine for the supported subset).
- ✅ Kind-changing modifiers (`enum`/`annotation`/`sealed`/`data`/`value`/`object`/…) and
  semantics-changing ones (`tailrec`/`external`) are deliberately **not** skipped, so such
  declarations stay cleanly unsupported (skipped, never miscompiled).
- ✅ Conformance fixes (caught by the box harness): a `data class` that manually declares
  `equals`/`hashCode`/`toString`/`copy`/`componentN` no longer gets a duplicate synthesized member;
  `.toString()` on a *reference* receiver now `invokevirtual`s the real `toString` (was a no-op).
- ✅ Box conformance: **31 compiled / 31 OK / 0 FAIL** (up from 27); full suite 96 green.

## Phase 15 — Top-level `val`/`var` properties  ✅
- ✅ Data-driven (≈416 first-errors). Top-level properties → a `private static` backing field
  (`final` for `val`) + `public static final getX`/`setX` accessors on the file facade, initialized
  in `<clinit>`. References resolve to `getstatic`/`putstatic`; ABI matches kotlinc.
- ✅ `Package.property` (f4) metadata (name/type/flags/JVM-sig; `val`=8710, `var`=1798) so a Kotlin
  consumer can `import` the properties — verified round-trip (`hi:6`). `tests/top_level_property_e2e.rs`.
- ✅ Conformance fixes (box harness): `Unit`/unknown-typed properties (`val x = unitCall()`) are
  rejected (no void-descriptor field → no stack underflow); the harness now also skips `// MODULE:`
  multi-module tests (out of single-translation-unit scope).
- ✅ Box conformance: **34 compiled / 34 OK / 0 FAIL** (up from 31); full suite 97 green.

## Phase 16 — kotlinc-aligned diagnostics  ✅
- ✅ Error messages now match kotlinc's wording (the `file:line:col: error:` format already matched):
  `unresolved reference: x` (was `… 'x'`; also for unknown types), `type mismatch: inferred type is
  A but B was expected`, `val cannot be reassigned`, `conflicting declarations: x`.
- ✅ `tests/diagnostics_match_kotlinc.rs` compiles erroneous snippets with **both** krusty and the
  real kotlinc and asserts the first `error:` text is identical.

## Phase 17 — `object` declarations (singletons)  ✅
- ✅ `object Name { fun … }` → a class with a `public static final INSTANCE`, a **private**
  constructor, member functions (instance methods), built in `<clinit>` (`new`/`putstatic`).
  `Name.member(args)` lowers to `getstatic INSTANCE` + `invokevirtual`. ABI matches kotlinc.
- ✅ Class `@Metadata` flags = 326 (the `object` bit) so a Kotlin consumer sees it as an object —
  round-trip verified (`Math2.sq(7)`). `tests/object_e2e.rs` (shape + JVM run + kotlinc consume).
- ✅ Full suite 99 green; box conformance 34 / 34 OK / 0 FAIL.

## Phase 18 — `Char` type + char literals  ✅
- ✅ `'x'` char literals (with escapes) and the `Char` type (JVM descriptor `C`, handled with int
  instructions). Comparison/equality (`if_icmp`), concat (`append(C)`), `toString` (`valueOf(C)`),
  char-typed params/returns/locals. ABI matches kotlinc.
- ✅ Conformance fix: the typechecker's `resolve_ty` now also rejects **nullable primitives**
  (`Char?`/`Int?`/… need boxing) — previously it ignored `?` on a local, letting `a!!` run `ifnonnull`
  on an int (`kt4251` VerifyError). Now such files are cleanly skipped.
- ✅ `tests/char_e2e.rs` (JVM run + ABI vs kotlinc); full suite 103 green; box 33 / 33 OK / 0 FAIL.

## Phase 19 — Java interop breadth: construction + instance methods  ✅
- ✅ Construct a classpath Java object (`val c = util.Calc(10)`) → `new` + `invokespecial <init>`
  (constructor resolved via the `.class` reader by arg descriptors), typed `Ty::Obj(internal)`.
- ✅ Call **instance methods** on a classpath Java object (`c.add(5)`, `c.tag()`) → `invokevirtual`
  (method resolved via the reader). Java now covers: static calls, instance calls, construction,
  from loose dirs **and** jars; plus `java.lang.String` instance methods.
- ✅ `println(Char)` → `(C)V`. `tests/java_instance_e2e.rs` (real javac class, construct + call,
  `-Xverify:all`). Full suite 104 green; box 33 / 33 OK / 0 FAIL.
- ⬜ Remaining Java: JDK types via jimage, instance methods in signatures (needs per-file imports in
  Stage C), overload widening, `.java` source front end.

## Phase 20 — `enum class`  ✅
- ✅ (v0) `enum class Name { A, B }` → a class extending `java/lang/Enum`: one `public static final`
  field per entry, a private `(String,int)` constructor calling `Enum.<init>`, and a `<clinit>`
  constructing each entry. `Name.ENTRY` → `getstatic`; `==` (reference); `.name`/`.ordinal` →
  `java.lang.Enum` accessors. `@Metadata` flags=32902 + `enum_entry` (f13) so Kotlin consumers
  resolve the entries.
- ✅ Conformance fixes (box harness): `val u: Unit = when(...)` no longer emits a `Unit` store
  (stack underflow); a `when` arm that diverges (`return`) no longer emits a dead `goto` to method
  end (`Expecting a stackmap frame` VerifyError).
- ✅ `tests/enum_e2e.rs` (shape + JVM run incl. `.name`/`.ordinal`). Box conformance: **39 / 39 OK /
  0 FAIL** (up from 33); full suite 104 green.
- ⬜ Deferred (Kotlin-consumer parity): `values()`/`valueOf()`/`$VALUES`, the `kotlin/Enum<T>`
  generic supertype in metadata (so consumers get `.ordinal`), entry constructor args + bodies.

## Phase 21 — Interfaces (declarations + implementing classes)  ✅
- ✅ `interface Name { fun sig(): T }` → a JVM `public interface` (`ACC_INTERFACE|ABSTRACT`) with
  `public abstract` methods (no bodies); super-interfaces supported. `@Metadata` flags=102 + the
  abstract members.
- ✅ Supertype lists: `class C(...) : I1, I2 { … }` → the class `implements` those interfaces
  (`ClassWriter` gained an interfaces list + abstract methods + settable access). A base-class
  supertype (`: Base()`) is detected and cleanly **rejected** (v0 has no class inheritance →
  skipped, never miscompiled).
- ✅ Concrete-type dispatch (`Square(3).area()`) works via the class's own methods; ABI shows
  `implements Shape`. `tests/interface_e2e.rs` (shape + JVM run). Full suite 106 green; box 39/39
  OK/0 FAIL.
### 21b — interface-typed polymorphism ✅
- ✅ A value typed as an interface (`val s: Shape = Square(3)`, or an interface-typed parameter)
  dispatches via **`invokeinterface`** (new `InterfaceMethodref` constant + opcode). A class is
  **assignable to an interface it implements** (`expect_assignable` subtyping), so `describe(Rect(..))`
  for `fun describe(s: Shape)` type-checks and runs. `tests/interface_e2e.rs::interface_polymorphism_runs`.
- ⬜ Deferred: class inheritance (`: Base()` — needs open/abstract + super-ctor), default interface
  methods, generics.

## Phase 22 — Class inheritance  ✅
- ✅ `open`/`abstract` classes are emitted non-`final` (`abstract` adds `ACC_ABSTRACT`); their
  members are non-`final` so subclasses can override. `class Sub(...) : Base(args)` → JVM `extends`,
  the primary constructor calls `super(args)` (args lowered through a constructor `MethodEmitter`).
- ✅ Inherited methods/properties resolve up the base-class chain (`SymbolTable::method_of`/
  `prop_of`); subtyping (`obj_is_subtype`) walks supers + interfaces; `invokevirtual` resolves
  inherited members.
- ✅ Conformance fix (box harness): an `open` class's overridden method was emitted `final`
  (`IncompatibleClassChangeError` when subclassed) — fixed.
- ✅ `tests/inheritance_e2e.rs` (super-ctor with args + inherited method + inherited property).
  Box conformance: **46 / 46 OK / 0 FAIL** (up from 39); full suite 109 green.
- ⬜ Deferred: `override`-flagged virtual re-dispatch nuances, abstract methods in classes,
  generics.

## Phase 23 — String templates  ✅ (biggest single conformance jump)
- ✅ Data-driven: `"$x"`/`"${…}"` was the #1 first-error (≈860 files). The lexer now expands an
  interpolated string into inline tokens (`TemplateStart StrChunk (Dollar Ident | Dollar { expr })*
  TemplateEnd`) via a token queue + `lex_one`, so `${expr}` parses into the same AST arena (no
  cross-arena copying). `Expr::Template` lowers to `StringBuilder.append(...)` per part; ABI matches
  kotlinc.
- ✅ Fix: `emit_append` appended `Boolean` via `append(I)` (`0/1`) — corrected to `append(Z)`
  (`true/false`), which templates/concat rely on.
- ✅ `tests/string_template_e2e.rs` (JVM run + ABI vs kotlinc). Box conformance: **62 / 62 OK /
  0 FAIL** (up from 46); full suite 110 green.

## Phase 24 — Class-body properties, plain ctor params, `init` blocks  ✅
- ✅ Class bodies accept `val`/`var` **properties** (backing field + accessor, initialized in the
  primary constructor) and `init { }` blocks; both run in source order after the ctor-param stores.
- ✅ **Plain (non-property) primary-constructor parameters** (`class C(start: Int)`) — in scope for
  `init`/body-property initializers, not fields. `ClassSig` now separates `ctor_params` (full
  signature) from `props` (backing fields); construction uses `ctor_params`.
- ✅ Conformance fixes (box harness): an `open` property read inside its class now dispatches through
  the (virtual) getter so overrides win (`kt1170`); colliding accessor names (case-only-differing,
  `@JvmField`-style) are rejected instead of emitting a duplicate method (`kt12189`).
- ✅ `tests/class_body_e2e.rs` (body props + `init` + plain param; open-property dispatch).
  Box conformance: **67 / 67 OK / 0 FAIL** (up from 62); full suite 112 green.

## Phase 25 — Safe calls (`?.`)  ✅
- ✅ `recv?.prop` and `recv?.method(args)` lower to a null-guard: evaluate the receiver, `ifnull` →
  push `null`, else do the member access / call. Works on krusty classes (incl. interfaces →
  `invokeinterface`), `java.lang.String`, and classpath Java objects; composes with `?:`.
- ✅ Result is reference-typed (krusty doesn't box) — a non-reference safe-call result is rejected
  (skipped, not miscompiled).
- ✅ `tests/safe_call_e2e.rs` (safe method + property, with Elvis). Full suite 114 green; box
  conformance 67 / 67 OK / 0 FAIL.

## Phase 26 — Generics via type erasure  ✅
- ✅ Parse-tolerate type-parameter lists (`class Box<T>`, `fun <T, U> …`) and the modifiers/bounds
  inside them (`reified`, `out`/`in`, `: Bound`), plus type *arguments* on types (`List<String>`)
  — all skipped syntactically (`parse_type_params`, `skip_type_args`).
- ✅ Erase every type-parameter reference to `java/lang/Object` in both the resolver and codegen
  (`Checker.tparams`, `resolve_ty`; emit's `resolve_ty` falls back to `Object`). This matches the
  bytecode kotlinc emits — a generic getter is `()Ljava/lang/Object;`, a generic param is `Object`.
- ✅ Any reference type is assignable to an erased `T` (= `Object`); a value flowing *out* of `T`
  into a more specific type would need a synthetic `checkcast` (not modelled) and is rejected, never
  miscompiled. Nullable/primitive-over-generic cases likewise skip.
- ✅ Overloads that collide after erasure (`<T> f(T)` vs `<U> f(U)` → both `f(Object)`) are rejected
  with a "conflicting overloads … after type erasure" diagnostic — kotlinc keeps them distinct by
  erasing each parameter to its *bound*, which krusty does not model, so we skip rather than emit a
  duplicate method (`ClassFormatError`). Checked for top-level functions and class methods.
- ✅ `tests/generics_e2e.rs` (generic class + inferred generic call run on the JVM; erased-getter
  ABI assertion; erased-overload-clash rejection). Full suite green; box conformance **70 OK / 0
  FAIL** (generic declarations + inferred usage now compile).

## Phase 27 — Type tests & casts (`is` / `!is` / `as` / `as?`)  ✅
- ✅ `e is T` / `e !is T` lower to `instanceof` (→ `Boolean`, negated via `^ 1`); `e as T` to
  `checkcast`; `e as? T` to an `instanceof`-guarded cast (value kept on match, `null` otherwise).
  `is` is parsed as a named-check at comparison precedence, `as`/`as?` at postfix precedence.
- ✅ `Any` is recognized as `java/lang/Object`. A primitive→`Any` assignment is now correctly
  *rejected* (krusty doesn't box) rather than silently storing an unboxed primitive.
- ✅ Operand and target must be *known reference types*: an unresolved target (`Number`, a value
  class, `Nothing`, an erased type parameter) would degrade to `instanceof Object`/`checkcast
  Object` (a no-op / always-true) — rejected, not miscompiled. Nullable `is T?` (where `null is T?`
  is true but `instanceof` is false) is rejected. `String` uses its real internal name.
- ✅ No smart-casting yet (explicit `as` covers the common idiom); a follow-up.
- ✅ **Bridge methods.** Recognizing `Any` exposed latent bridge bugs. krusty now rejects any class
  whose *effective* implementation of a declared-supertype method (own or inherited up the base
  chain — incl. *fake overrides* where the impl is inherited and the differing signature comes from
  an interface) has the same erased parameters but a different return descriptor, and any data class
  overriding a synthesized `copy`/`componentN` via an interface — these need a JVM bridge krusty
  doesn't emit (`AbstractMethodError`).
- ✅ **Null-safe `data class` equals.** Reference fields now compare via `java.util.Objects.equals`
  (a nullable field could be `null` → a plain `.equals` would NPE).
- ✅ `tests/is_as_e2e.rs` (is/!is/as/as? run on the JVM; unsafe-cast rejection). Box conformance
  **77 OK / 0 FAIL** (up from 70).

## Phase 28 — Smart-casting  ✅
- ✅ After `if (x is T) { … }`, a stable `x` (a `val` or parameter) is narrowed to `T` inside the
  then-branch; `if (x !is T) … else` narrows it in the else-branch; and an early-return guard
  `if (x !is T) return …` (a diverging then-branch, no else) narrows it for the rest of the block.
- ✅ A `var` is never smart-cast (it could be reassigned) — the member access stays unresolved.
  Only non-nullable, known reference targets narrow (consistent with the `is`/`as` rules).
- ✅ Codegen inserts a `checkcast` to the narrowed type when loading the narrowed local (the slot
  still holds the wider type), so member dispatch and the JVM verifier agree.
- ✅ `tests/smartcast_e2e.rs` (if-then + early-return guard on the JVM; `var` non-narrowing). Box
  conformance **80 OK / 0 FAIL** (up from 77).

## Phase 29 — `when` type-test arms  ✅
- ✅ Subject-form `when (x) { is T -> … }` parses `is T` / `!is T` arms into a type test against the
  subject; codegen dispatches via `instanceof` on the subject slot (evaluated once, not re-emitted),
  branching with `ifne`/`ifeq`.
- ✅ The checker skips the `==`-comparability constraint for type-test arms, and smart-casts the
  subject to `T` inside a single positive `is T` arm's body (reusing the Phase 28 machinery).
- ✅ `tests/when_is_e2e.rs` (sealed-style dispatch + per-arm smart-cast on the JVM). Box conformance
  holds at **80 OK / 0 FAIL** (exhaustive `when` without `else` over sealed types — needed for many
  such files to fully compile — is a separate follow-up).

## Phase 30 — Raw string literals  ✅
- ✅ `"""..."""` lexes as a single `StringLit` whose content is verbatim — no escape processing
  (`\n` is backslash-n), may span lines, and may contain one or two consecutive quotes. The closing
  delimiter is a run of three quotes (a longer run leaves the surplus quotes in the content).
- ✅ Interpolation inside a raw string (`$x` / `${…}`) is not yet supported and is rejected (skipped)
  rather than mis-lexed as literal text.
- ✅ `tests/raw_string_e2e.rs` (multi-line + embedded quotes run on the JVM; verbatim value;
  interpolation rejection). Box conformance **81 OK / 0 FAIL** (up from 80).

## Phase 31 — Exhaustive `when` over sealed types  ✅
- ✅ `sealed` is now tracked through `ClassDecl` → `ClassSig` (`is_sealed`). A subject `when` with no
  `else` is treated as an expression (value = join of arm bodies) when the subject is a sealed class
  and every declared subclass is matched by a positive `is` arm (`SymbolTable::subclasses_of`).
- ✅ Conservative: a non-sealed subject, any uncovered subclass, or a nested sealed subclass not
  directly matched ⇒ not exhaustive ⇒ the `when` stays `Unit` and using it as an expression is
  rejected (skipped), never assumed exhaustive.
- ✅ Codegen emits the unreachable no-match path as a `throw new IllegalStateException()` (mirroring
  Kotlin's `NoWhenBranchMatchedException`; a plain JDK exception avoids a stdlib dependency) so the
  verifier sees every path produce a value or diverge.
- ✅ `tests/when_exhaustive_e2e.rs` (exhaustive sealed dispatch on the JVM; non-exhaustive rejection).
  Box conformance holds at **81 OK / 0 FAIL** (removes a class of false rejections; sealed-`when`
  box files typically need further features to fully compile).

## Phase 32 — `throw` + JDK exceptions  ✅
- ✅ `throw e` is a prefix expression of bottom type `Ty::Nothing` (added to the type model): the
  bottom type is assignable to every type, joins to the *other* branch (`if (c) x else throw e` is
  typed `x`), and never yields a value (codegen emits `athrow`). `Nothing` and `throw` are folded
  into the divergence analysis so dead jumps after a throwing branch are skipped.
- ✅ Common JDK exceptions construct by simple name (`RuntimeException("msg")`,
  `IllegalStateException()`, `IllegalArgumentException`, `AssertionError`, … — `builtin_exception`),
  with the no-arg and single-`String` constructors, so `throw RuntimeException(...)` needs no import.
- ✅ Fixed a latent miscompile this exposed: `inline`/`value class` (unboxed semantics) was being
  compiled as a normal class (wrong `==`) — now rejected (skipped).
- ✅ `tests/throw_e2e.rs` (throw as guard/body, exception thrown with message preserved, on the JVM;
  inline-class rejection). Box conformance **86 OK / 0 FAIL** (up from 81).

## Phase 33 — `try`/`catch`  ✅
- ✅ Added a `Code` exception table to the class-file writer (`CodeBuilder::add_exception` resolves
  label offsets in `link`). `try { body } catch (e: T) { … } …` guards the body range; each handler
  enters with the exception on the stack (`set_stack(1)`), stores it into the catch variable's slot,
  binds the variable for the handler body, and produces the result. Multiple catches dispatch in
  declaration order (place the subtype first). `try` is an expression (value = body or a catch body).
- ✅ Catch types resolve via `catch_internal` (a JDK exception / import / declared class); an
  unresolvable catch type is rejected. `finally` is rejected (needs duplicated-block lowering).
- ✅ Soundness guard: a `try` is only emitted where the operand stack is empty at entry (statement,
  initializer, `return`, argument). Elsewhere (`"" + try { … }`) an exception unwind would clear
  partially-computed stack values, so it is rejected (skipped) — never miscompiled.
- ✅ `tests/try_catch_e2e.rs` (try-as-expression + multi-catch hierarchy on the JVM; stack-nonempty
  and `finally` rejection). Box conformance **91 OK / 0 FAIL** (up from 86).

## Phase 34 — Explicit `this` + member assignment  ✅
- ✅ `this` resolves to the enclosing class type (the checker tracks `this_ty`); codegen loads it as
  `aload 0` in instance context. Usable as a value (`return this`), a receiver (`this.foo()`), and a
  member read (`this.v`).
- ✅ Member assignment `receiver.prop = value` (and compound `receiver.prop += value`) writes via the
  property's public setter — backing fields are private, so a cross-instance `putfield` would fail,
  and the setter also dispatches correctly for open classes. Assigning a `val` member is rejected.
- ✅ `tests/this_member_e2e.rs` (this read/receiver + cross-instance and compound member assignment on
  the JVM; `val`-member rejection). Box conformance **99 OK / 0 FAIL** (up from 91; 100 compiled).

## Phase 35 — Arrays  ✅
- ✅ Added `Ty::Array(&'static Ty)` (element types interned via `intern_ty` so equal arrays compare
  by value) with descriptor `[<elem>`. Type syntax: `IntArray`/`LongArray`/`DoubleArray`/
  `BooleanArray`/`CharArray` and `Array<T>` (the element type arg is captured on `TypeRef`); an
  `Array` of a primitive (would box) is rejected.
- ✅ Creation builtins: `intArrayOf(…)`/`charArrayOf(…)`/… (typed `newarray` + per-element store),
  `arrayOf(…)` (element = common reference type of the args → `anewarray`), and the size constructors
  `IntArray(n)`/… (zero-filled). `arrayOf` of a primitive is rejected (use `intArrayOf`).
- ✅ Element read `a[i]` and write `a[i] = v` (and compound `a[i] += v`) select the right
  `Xaload`/`Xastore` opcode per element type; `a.size` → `arraylength`.
- ✅ `is`/`as` to an array type use the array *descriptor* (`[LData;`, `[I`) as the operand — fixing a
  verify failure where `(arr as Array<Data>)[0]` cast to `Object` then `aaload`'d a non-array.
- ✅ `tests/array_e2e.rs` (primitive + reference arrays, read/write/compound/`.size`/iteration on the
  JVM; `arrayOf`-of-primitive rejection). Box conformance **104 OK / 0 FAIL** (up from 99).

## Phase 36 — `super` calls  ✅
- ✅ `super.method(args)` resolves to the base class's method (via `method_of` up the declared chain)
  and emits `aload 0; args; invokespecial Super.method` — non-virtual dispatch, so an `override` can
  delegate to the implementation it overrides. A `super` method krusty can't resolve to a declared
  supertype is rejected.
- ✅ `tests/super_call_e2e.rs` (override delegating via `super`, called both directly and through the
  base-typed reference, on the JVM). Box conformance **105 OK / 0 FAIL** (up from 104).

## Phase 37 — `Float` + numeric conversions  ✅
- ✅ `Ty::Float` (descriptor `F`, promotion rank Int<Long<Float<Double): literal lexing `1.5f`/`1f`
  (and an optional `d`/`D` on a Double), `Expr::FloatLit`, and the full `fload`/`fstore`/`freturn`/
  `fadd`/`fsub`/`fmul`/`fdiv`/`frem`/`fneg`/`fcmpg` opcode set + `CONSTANT_Float`. Float flows through
  fields, params/returns, comparison, string templates/`toString`/`println`, and data-class
  `equals`/`hashCode`.
- ✅ Numeric conversions `n.toInt()`/`toLong()`/`toFloat()`/`toDouble()` on any numeric receiver,
  emitting the right `i2f`/`l2i`/`f2d`/`d2i`/… opcode (no-op when source == target).
- ✅ Fixed a latent miscompile this exposed: elvis `?:` and `!!` on a *non-null primitive*
  (`42 ?: 239`, `n!!`) were emitting `ifnonnull` on a non-reference (verify failure); they are now
  the operand itself, matching kotlinc.
- ✅ `tests/float_e2e.rs` (Float arithmetic/comparison/fields, conversions, primitive elvis/`!!` on
  the JVM). Box conformance **109 OK / 0 FAIL** (up from 105).

## Phase 38 — `companion object`  ✅
- ✅ `companion object { fun…; const val/val… }` members are emitted as `static`/`static final`
  members of the enclosing class: `ClassName.fn(...)` → `invokestatic`, `ClassName.PROP` →
  `getstatic` (+ a `<clinit>` for property initializers). Members are also reachable *unqualified*
  inside other companion members (tracked via `companion_of` in the checker and emitter).
- ✅ Scope/soundness (krusty puts statics on the *same* class, not a nested `Companion`): a companion
  member whose name collides with an instance member is rejected (would duplicate a field/method),
  and a companion member that reads/writes a top-level property is rejected (it would target the
  wrong class). The ABI differs from kotlinc's nested-`Companion` shape but executes correctly.
- ✅ `tests/companion_e2e.rs` (qualified + unqualified static members on the JVM; collision rejection).
  Box conformance **110 OK / 0 FAIL** (up from 109).

## Phase 39 — `break` / `continue`  ✅
- ✅ Unlabeled `break`/`continue` (soft keywords) in `for`/`while`. Codegen tracks a stack of
  `(continue_target, break_target)` labels per loop: `break` → past the loop, `continue` → the loop's
  step (in a `for`, the counter still advances — `continue` targets a label bound before the
  increment). `break`/`continue` outside a loop is rejected.
- ✅ `tests/break_continue_e2e.rs` (break + continue in for and while on the JVM; outside-loop
  rejection). Box conformance **113 OK / 0 FAIL** (up from 110).

## Phase 40 — `Byte` / `Short`  ✅
- ✅ `Ty::Byte` (`B`) and `Ty::Short` (`S`): int on the JVM stack, so they reuse the int opcode arms
  (`iload`/`istore`/`ireturn`/`if_icmp`/append-as-`(I)`/…). Arithmetic promotes to `Int`
  (`promote` maps a Byte/Short result to Int — Kotlin has no byte/short arithmetic). An integer is
  assignable to Byte/Short (literal narrowing); `emit_expr_as` now narrows via `i2b`/`i2s`.
- ✅ Conversions `.toByte()`/`.toShort()` truncate (source → `Int` → `i2b`/`i2s`), e.g.
  `130.toByte()` == -126.
- ✅ Fixed a latent miscompile this exposed: a `Char` field in a `data class` fell to the
  `Objects.equals`/`Object.hashCode` *reference* path (passing a primitive char as `Object` →
  verify failure); `Char` now uses `if_icmpeq`/`Integer.hashCode` like the other int-category types.
- ✅ `tests/byte_short_e2e.rs` (literals, arithmetic→Int, truncating conversions, fields, comparison,
  data-class equals incl. a Char field, on the JVM). Box conformance **116 OK / 0 FAIL** (up from 113).

## Phase 41 — `try`/`finally`  ✅
- ✅ `finally` is inlined on the normal path (after the body) and after each normally-completing
  catch, plus a synthetic catch-all (exception-table entry, `catch_type` 0) over the body and the
  catch bodies that runs the finally then re-throws the in-flight exception.
- ✅ Soundness: a `return`/`break`/`continue` that escapes the guarded region bypasses the inlined
  finally, so such trys are rejected (a deep `exit_walk` treats `return` as always-escaping and
  `break`/`continue` as escaping only when not inside a loop nested in the region, recursing into
  nested `try`). `finally` requires a Unit/Nothing body (no value to thread across it); otherwise
  rejected.
- ✅ Empty/degenerate exception-table ranges (`start >= end`, e.g. an empty `try {}` body) are
  dropped in `resolved_exceptions` — they protect nothing and are an illegal `Code` entry.
- ✅ `tests/try_finally_e2e.rs` (finally on normal, caught, and re-thrown paths on the JVM). Box
  conformance **128 OK / 0 FAIL** (up from 116).

## Phase 42 — `lateinit`  ✅
- ✅ A property may now be declared without an initializer (`PropDecl.init: Option`); `lateinit var
  x: T` emits a backing field left at its default (null) and assigned later. Reads of a `lateinit`
  property emit a null-check that throws (a `RuntimeException` standing in for the stdlib
  `UninitializedPropertyAccessException`, caught the same way) — at implicit-`this`, explicit
  `recv.prop`, qualified `Class.PROP`, and unqualified companion reads.
- ✅ A no-initializer property that isn't `lateinit` (an `abstract`/interface property) is rejected —
  this also fixed a regression where such a property let an `abstract` class compile and then hit a
  separate free-function-from-`init` issue.
- ✅ `tests/lateinit_e2e.rs` (set-then-read, read-before-init throws, on the JVM; abstract-property
  rejection). Box conformance **132 OK / 0 FAIL** (up from 128).

## Phase 43 — Interface properties  ✅
- ✅ Abstract interface properties (`val`/`var x: T`, no initializer/getter) → abstract `getX`
  (and `setX` for `var`) on the interface; implementing classes provide them via their own property
  accessors. Access through an interface-typed value dispatches via `invokeinterface` (read and
  write). Registered in the interface's `ClassSig.props`/metadata for resolution.
- ✅ Interface default methods (a `fun` with a body) are rejected — they need a Java-8 interface
  (`default` keyword; krusty emits v52 but doesn't yet model JVM default interface methods). A
  property with an initializer/custom getter is likewise rejected.
- ✅ Extended bridge detection to *property getters*: a supertype property whose erased type differs
  from the class's own (a generic interface `val x: T` → `Object` overridden with a concrete type)
  needs a bridge `getX` krusty doesn't synthesize → rejected (`supertype_internals` helper).
- ✅ `tests/interface_property_e2e.rs` (interface val/var read+write through an interface-typed value
  on the JVM; default-method rejection). Box conformance **137 OK / 0 FAIL** (up from 132).

## Phase 44 — Enum constructors + hex/binary literals  ✅
- ✅ Enum classes with a primary constructor and per-entry arguments
  (`enum class Color(val rgb: Int) { RED(0xFF0000), … }`): `enum_entry_args` (parallel to
  `enum_entries`); the `<init>` takes `(String name, int ordinal, <ctor params>)`, `<clinit>`
  constructs each entry `new C("NAME", ordinal, args…)`, and property params become fields + getters.
  Member functions after the `;` are emitted as instance methods. Per-entry class bodies
  (`RED { … }`, an anonymous subclass) are rejected.
- ✅ Hex (`0xFF`), binary (`0b1010`), and `_`-separated integer literals (lexer + `parse_int_literal`,
  via `u64` so `0xFFFFFFFF` fits, with the `L` long suffix preserved).
- ✅ `tests/enum_args_e2e.rs` (enum ctor + per-entry args + methods + `name`/`ordinal`, and
  hex/binary/underscore literals, on the JVM). Box conformance **139 OK / 0 FAIL** (up from 137).

## Phase 45 — `for` over arrays  ✅
- ✅ `for (x in array)` (a `Stmt::ForEach`) is lowered to an index loop: store the array + an index,
  loop while `i < arr.length`, bind `x = arr[i]` (the right `Xaload` per element type), `iinc` the
  index. Works for primitive and reference arrays and composes with `break`/`continue` (continue →
  the increment). Iterating a non-array (string, range object, collection) is rejected.
- ✅ `tests/foreach_e2e.rs` (primitive + reference array iteration with break/continue on the JVM;
  non-array rejection). Box conformance **147 OK / 0 FAIL** (up from 139).

## Phase 46 — `vararg` parameters  ✅
- ✅ A `vararg xs: T` parameter (captured via `Param.is_vararg`, `Signature.vararg`) has runtime type
  `Array<T>`; the body sees `xs` as the array. Callers of a vararg free function match fixed
  parameters by position, then pack the trailing arguments into a fresh array (the right element
  type / `Xastore`) — including zero trailing args (an empty array). `*spread` is not supported.
- ✅ `tests/vararg_e2e.rs` (vararg sum/join with a leading fixed param and zero/var args, on the JVM).
  Box conformance holds at **147 OK / 0 FAIL** (also removes a latent mis-handling where `vararg` was
  silently skipped and the parameter mis-typed as its element type).

## Phase 47 — String iteration  ✅
- ✅ `for (c in str)` iterates a String's characters (`c: Char`), lowered to an index loop over
  `String.length()` / `String.charAt(i)` (the same `ForEach` machinery as arrays, so it composes
  with `break`/`continue`). Non-array / non-String iterables remain rejected.
- ✅ (Verified `when` with comma conditions — `1, 2, 3 -> …` — already works via the existing
  multi-condition arm.)
- ✅ `tests/string_iter_e2e.rs` (char counting, accumulation, break, on the JVM). Box conformance
  **148 OK / 0 FAIL** (up from 147).

## Phase 48 — Computed properties  ✅
- ✅ A class property with a custom getter (`val x: T get() = expr` / `get() { … }`) and no
  initializer is a *computed property*: no backing field, no constructor init — krusty emits a
  `getX()` method running the getter body (instance method, implicit-`this` scope), and the checker
  type-checks the getter body against the property type. Reads (`r.x`) already route through `getX`.
- ✅ Top-level computed properties are rejected (the facade emits a backing field, not a getter — it
  would miscompile). A computed property requires a type annotation (no getter-return inference yet).
- ✅ `tests/computed_prop_e2e.rs` (expression + block getters reading other props, on the JVM). Box
  conformance **149 OK / 0 FAIL** (up from 148).

## Phase 49 — Precondition intrinsics + non-null cast check  ✅
- ✅ Stdlib precondition intrinsics (when not shadowed by a user function): `require(cond)` →
  `IllegalArgumentException`, `check(cond)` → `IllegalStateException`, `assert(cond)` →
  `AssertionError` (all → `Unit`); `error(msg)` → `throw IllegalStateException(msg)` and `TODO()`/
  `TODO(msg)` → `throw RuntimeException` (both `Nothing`). Added `emit_string_of` to coerce a message
  of any type to `String`.
- ✅ `x as T` to a *non-nullable* `T` now throws on a null value (Kotlin's cast null check) — bare
  `checkcast` let null through, so `null as TestKlass` wrongly succeeded; `x as T?` still keeps null.
- ✅ A `try` used as a statement no longer requires its body/catches to share a type (lenient merge →
  `Unit`); only an expression use that needs a value is constrained.
- ✅ `tests/preconditions_e2e.rs` (require/check/error + non-null-cast throw on the JVM). Box
  conformance **153 OK / 0 FAIL** (up from 149).

## Phase 50 — Curated `StringBuilder`  ✅
- ✅ `StringBuilder()` / `StringBuilder("init")` / `StringBuilder(capacity)` construction, chained
  `append(x)` (any primitive/String/reference → returns the builder, `invokevirtual`), `toString()`,
  and the `.length` property (`length()`). Resolved via `resolve_stringbuilder_instance` (mirrors the
  curated `java.lang.String` resolver). Not shadowable by a user function of the same name.
- ✅ `tests/stringbuilder_e2e.rs` (construction, chained append of mixed types, `toString`, `.length`,
  on the JVM). Box conformance holds at **153 OK / 0 FAIL** (StringBuilder-heavy box tests typically
  need further stdlib surface to fully compile; this removes the construction blocker).

## Phase 51 — `object` bodies with properties  ✅
- ✅ `object` bodies now accept `val`/`var`/computed properties and `init` blocks (in addition to
  `fun`): backing fields + accessors on the singleton, initialized in its `<init>` (run from
  `<clinit>` when `INSTANCE` is built). `ObjectName.prop` reads via `getstatic INSTANCE;
  invokevirtual getProp()` (checker + codegen). Optional supertype list is tolerated.
- ✅ Fixed a latent miscompile this exposed: a top-level property *write* from an instance method /
  `init` block was silently dropped (it would target the class, not the facade) — now rejected, like
  the read path (`const val` not-triggering-init semantics aren't modeled, so such files skip).
- ✅ `tests/object_props_e2e.rs` (object val/var/computed + mutation via a method, on the JVM). Box
  conformance **158 OK / 0 FAIL** (up from 153).

## Phase 52 — Lambdas (inlined `let`/`also`)  ✅
- ✅ Lambda literals `{ param -> body }` / `{ body }` (single optional parameter, default `it`;
  `Expr::Lambda`) parse as a trailing argument (`expr { … }` / `recv.m(args) { … }` appends the
  lambda as the last call argument, same line).
- ✅ The scope functions `recv.let { … }` and `recv.also { … }` are *inlined* (no anonymous class):
  the receiver is stored to a local bound to the lambda parameter; `let` yields the body's value,
  `also` the receiver. Foundational lambda infrastructure for future `run`/`with`/`apply`.
- ✅ A lambda anywhere other than a `let`/`also` argument is rejected (checker + codegen).
- ✅ `tests/scope_fn_e2e.rs` (let/also with `it`/named param, member access, mutation, chaining, on
  the JVM; lambda-misuse rejection). Box conformance holds at **158 OK / 0 FAIL** (`run`/`with`/
  `apply` — which rebind `this` — and higher-order functions are the next lambda steps).

## Phase 53 — `package` after annotations + `typealias` skip  ✅
- ✅ A `package` directive is now accepted in the top-level loop (not just as the very first token),
  so it parses after file-level annotations (`@file:JvmName(...)` etc.) — previously it cascaded into
  "expected a top-level declaration".
- ✅ `typealias Name = Type` is skipped (not modeled) instead of cascading; a file that actually
  *uses* the alias still fails to resolve it and is cleanly skipped.
- ✅ `tests/package_directive_e2e.rs` (package after `@file:` annotation + typealias, clean
  parse/check/emit into the package's facade). Box conformance **161 OK / 0 FAIL** (up from 158).

## Phase 54 — Unqualified intra-class method calls  ✅
- ✅ An unqualified call to a sibling instance method (`foo()` inside another method) now resolves to
  `this.foo()` and emits `aload 0; args; invokevirtual` (walking the base-class chain via
  `method_of`). Previously only `this.foo()` worked; bare `foo()` was an "unresolved function".
- ✅ `tests/intra_class_call_e2e.rs` (sibling + inherited method called unqualified, on the JVM). Box
  conformance **164 OK / 0 FAIL** (up from 161). Foundational for `run`/`with`/`apply` (which rebind
  the implicit receiver) — the next lambda step.

## Phase 55 — `run`/`with`/`apply` (implicit-receiver scope functions)  ✅
- ✅ `recv.run { … }` / `with(recv) { … }` (yield the body) and `recv.apply { … }` (yield the
  receiver) are inlined: the receiver is stored to a local and becomes the body's implicit receiver.
  Inside the body, `this` and unqualified member access (properties *and* methods) target the
  receiver — implemented via a `recv: Option<(slot, class)>` context on the emitter (`emit_implicit_this`
  / `implicit_class`) and a `check_with_receiver` in the checker (sets `this_ty`, brings the
  receiver's props into scope). Member reads/writes use the receiver's accessors (its fields are
  private to its own class).
- ✅ The `with(x) { }` form is intercepted before its arguments are type-checked (the trailing lambda
  isn't a normal value). A receiver lambda with an explicit parameter is not treated as run/with/apply.
- ✅ `tests/receiver_scope_fn_e2e.rs` (run/apply/with with unqualified method + property access and
  mutation, on the JVM). Box conformance holds at **164 OK / 0 FAIL** (completes the scope-function
  family; broader gains await higher-order functions / collections).

## Phase 56 — Compile-time `trimIndent`/`trimMargin`  ✅
- ✅ `"…".trimIndent()` / `"…".trimMargin()` are kotlin-stdlib extensions (no JDK method; krusty
  doesn't link the stdlib), so krusty *folds* them at compile time when the receiver is a string
  literal: `trimIndent` drops a blank first/last line then strips the minimum common leading
  whitespace; `trimMargin` strips each line up to the `|` marker. A non-literal receiver is rejected.
- ✅ `tests/trim_indent_e2e.rs` (both fold correctly on multi-line raw strings, on the JVM). Box
  conformance holds at **164 OK / 0 FAIL** (clears the #1 String-method blocker, 125 first-errors;
  those files have further blockers, so it compounds rather than landing alone).

## Phase 57 — `++`/`--` + null-safe reference `==`  ✅
- ✅ `++`/`--` (new `PlusPlus`/`MinusMinus` tokens), prefix and postfix, in statement position on a
  simple variable, desugared to `name = name ± 1`. `while` now parses a statement body (via
  `parse_branch`), so `while (c) i++` works. Increment on a non-variable is rejected.
- ✅ Fixed a latent miscompile this exposed: reference `==`/`!=` used `a.equals(b)` (NPE when `a` is
  null) instead of Kotlin's null-safe structural equality — now `java.util.Objects.equals(a, b)`
  (in both the comparison-jump and `when`-subject paths).
- ✅ `tests/inc_dec_e2e.rs` (pre/post inc/dec incl. a `while` body, and null-safe `==`, on the JVM).
  Box conformance **168 OK / 0 FAIL** (up from 164).

## Phase 58 — `for (i in arr.indices)`  ✅
- ✅ `for (i in X.indices)` desugars (in the parser) to the counted loop `0 until X.size` — an Int
  loop over the index range — reusing the existing range-`for` lowering (and `.size` →
  `arraylength`). Works for primitive and reference arrays.
- ✅ `tests/for_indices_e2e.rs` (index iteration over int and reference arrays, on the JVM). Box
  conformance holds at **168 OK / 0 FAIL** (those files have further blockers; compounds).

## Phase 59 — Unannotated computed-getter inference  ✅
- ✅ A computed property without a type annotation (`val x get() = expr`) now infers its type from the
  getter body (`infer_getter_ty`: literals, property/`this.x` references against the class's collected
  props, `.size`/`.length`, unary/binary ops) during signature collection. Emit uses the inferred
  type from the symbol table so `getX`'s descriptor matches callers (a getter whose body needs more
  than the light inferer covers stays `Error` → cleanly skipped).
- ✅ `tests/computed_getter_infer_e2e.rs` (inferred Int/Boolean/String getters, on the JVM). Box
  conformance holds at **168 OK / 0 FAIL** (clears 124 first-errors; those files have further
  blockers, so it compounds).

## Phase 60 — Default parameter values  ✅
- ✅ Free functions may declare default values (`fun f(x: Int = 5, y: String = "hi")`). The parser
  reads `= expr` after a parameter type; `Param` gains a `default` field. `Signature` gains
  `required` (the minimum arg count = params without a trailing default). A call may now supply
  `required..=params.len()` positional args; the checker type-checks each default against its
  param type, and the emitter fills omitted trailing params with their default expressions at the
  call site (the emitted method keeps the full parameter list).
- ✅ Correctness guards (keep the never-miscompile invariant):
  - A default that references *another parameter* can't be reproduced at the call site → rejected.
  - Defaults on object/companion/instance methods aren't call-site-filled yet, so a call that
    omits them is rejected (arity-checked), not miscompiled. (Caught 3 `jvmStatic` cases that a
    missing object-method arity check would otherwise have let through to a `VerifyError`.)
- ✅ `tests/default_args_e2e.rs` (literal/bool/top-level-val defaults, run on the JVM). Box
  conformance **168 → 170 OK / 0 FAIL**.

## Phase 61 — Annotations (parse + ignore)  ✅
- ✅ Annotation *uses* now parse anywhere they appear and carry no codegen meaning: the existing
  declaration-prefix path already skipped `@Anno(...)` on declarations/params; this phase adds
  skipping leading annotations on *statements* (`@Suppress("…") val x = …`, `@Suppress(...) for ...`)
  in `parse_stmt`.
- ✅ `annotation class Name(...)` declarations parse (via `parse_class`) and are then dropped — krusty
  emits no runtime representation for them. Using the annotation as a *value/type* then fails to
  resolve, so such a file is cleanly skipped (never miscompiled).
- ✅ `tests/annotations_e2e.rs` (annotation-class decl + `@Tag`/`@Suppress` uses on a function, a
  local, and a loop, run on the JVM). Box conformance **170 → 173 OK / 0 FAIL**.

## Phase 62 — Named arguments  ✅
- ✅ Top-level function calls accept named arguments (`f(b = 2, a = 5)`). The parser records a
  per-call `name =` label table on `File` (side-table keyed by the call's `ExprId`, no `Expr::Call`
  churn); `Signature` gains `param_names`. A shared `map_call_args` reorders source-order arguments
  onto positional parameter slots, validating unknown/duplicate names, positional-after-named, arity,
  and missing required parameters. Named args combine with omitted defaults.
- ✅ Evaluation order preserved: supplied arguments are spilled to fresh locals in *source* order,
  then loaded (or a default emitted) in *parameter* order — so a reordered call like
  `f(b = sideEffect(), a = sideEffect())` still evaluates `b` before `a` (verified on the JVM).
- ✅ Correctness guard: named arguments on anything other than a top-level function (methods,
  constructors, builtins) are rejected, since krusty doesn't reorder those — the labels would
  otherwise be silently ignored and miscompile.
- ✅ TDD: `tests/named_args_e2e.rs` (in-order / reordered / named+default / source-order eval, on the
  JVM) + a `named_arguments` checker unit test (accept + the two rejections). Gated by the full
  10,009-case original Kotlin `codegen/box` suite: **173 → 174 OK / 0 FAIL**.

## Phase 63 — kotlin.test assertions + latent-miscompile guards  ✅
- ✅ `kotlin.test` assertion intrinsics: `assertEquals(expected, actual[, msg])`, `assertTrue(cond[, msg])`,
  `assertFalse(cond[, msg])`. Each is `Unit`; a passing assertion is a no-op, a failing one throws
  `AssertionError`. `assertEquals` reuses the structural `==` emission (`emit_compare_jump`: primitive
  compares / null-safe `Objects.equals`). This was the single most common unresolved-function blocker.
- ✅ Unlocking ~50 new files surfaced **4 pre-existing latent miscompiles** (unrelated to assertions);
  all fixed by rejection to hold the never-miscompile invariant:
  1. **Local shadowing** — the emitter doesn't restore a shadowed slot mapping on block exit, so a
     nested `var x` aliased the outer slot (VerifyError). Reject a local that shadows an in-scope name.
  2. **Uninferrable property type** — an unannotated `var f = F(0)` inferred `Error` and emitted an
     erased `Object` getter while callers expected the concrete type (VerifyError). `infer_lit_ty` now
     also covers char/float/templates/unary/binary; a still-uninferrable initialized property is rejected.
  3. **Enum entry argument referencing a name** — emitted with the enum as the current class, so a
     top-level `val` ref resolved to the wrong owner (`NoSuchFieldError`). Reject name-bearing entry args.
  4. **Init-order edge (KT-73355)** — an `init` block calling a member method before a later property
     initializer. Reject.
- ✅ TDD: `tests/assertions_e2e.rs` (passing assertions are no-ops; a failing `assertEquals` throws,
  on the JVM) + `kotlin_test_assertions` and `rejects_latent_miscompiles` checker unit tests. Gated by
  the full 10,009-case original Kotlin `codegen/box` suite: **174 → 218 OK / 0 FAIL** (+44).

> Note: phases 64–69 (post-`assertions` work — `value`-as-param, supertype type-arg skipping,
> `fun interface`/class-delegation rejection, `in`/`out` variance + `Array<*>`, primitive type
> constants, `Nothing`-typed control flow, extension functions, classpath scanning) landed as
> commits but predate this plan being brought current; resume the running write-up from Phase 70.

## Phase 70 — `..<` (rangeUntil) operator  ✅
- ✅ Data-driven (the box `for`-loop survey showed `..<` as a recurring first-error in the
  "expected an expression"/"expected ')'" buckets). `..<` now lexes as a dedicated `DotDotLt`
  token (3-char, matched before `..`) and, in a `for` header, is treated exactly like `until`
  (`RangeKind::Until`) — so `for (i in a..<b)` and `for (i in a..<b step s)` lower to the existing
  half-open counted loop. ABI/codegen identical to the `until` form kotlinc emits.
- ✅ Range-as-value (`val r = a..<b`) remains out of subset (needs a real `IntRange` object), so a
  `..<` outside a `for` header is still cleanly rejected, never miscompiled.
- ✅ TDD: `tests/range_until_e2e.rs` (`0..<n` and `0..<n step 2` summed on the JVM). Full suite
  176 green. The `..<` files carry further blockers, so this compounds rather than landing alone.

## Phase 71 — Destructuring declarations (`val (a, b) = e`)  ✅
- ✅ Data-driven (the "expected loop variable"/"expected variable name" buckets surfaced `val (a, b)
  = …` and `for ((a, b) in …)` as the dominant shape). `val`/`var (a, b, …) = init` now parses to a
  new index-based `Stmt::Destructure { entries, init }`; each entry binds `init.componentN()`
  (1-based by position). An entry named `_` is skipped — no binding and no `componentN` call, per
  Kotlin.
- ✅ The checker resolves each `componentN` via `SymbolTable::method_of`, so destructuring works for
  any type that declares the operators — notably a krusty `data class` (which already synthesizes
  `component1..N`). A type without the operator (e.g. `String`, a non-data class) is rejected
  (`cannot destructure this type (no operator 'componentN')`), never miscompiled.
- ✅ Codegen evaluates the initializer once and keeps the receiver on the stack, `dup`-ing it for
  each component call and letting the last call consume it — so **no temp slot** is needed (a temp
  would otherwise have to be pre-allocated to satisfy a loop back-edge `StackMapTable` frame).
  `pre_alloc_loop_locals` also reserves the entry slots when a destructuring `val` is a top-level
  statement of a loop body, so destructuring inside `while`/`for` verifies.
- ✅ TDD: `tests/destructure_e2e.rs` (data-class destructuring with `_` skips, incl. inside a `for`
  loop, on the JVM; non-`componentN` type rejection). Full suite 178 green. `for ((a, b) in …)`
  destructuring loops (often over stdlib `withIndex()`/collections) remain a follow-up.

## Phase 72 — Stdlib/built-in type resolution via the classpath (no hardcoded lists)  ✅
- ✅ **Removed the hardcoded `builtin_exception` table.** Exception types now resolve from the
  classpath like any other: `Exception`/`RuntimeException`/`IllegalStateException`/… are kotlin
  **typealiases** read from `*TypeAliasesKt` `@Metadata` (`classpath::scan_types`), and `Throwable`
  is a built-in mapped type (below). A throwable is recognised structurally
  (`jvm::jvm_class_map::is_throwable_internal`: `…Exception`/`…Error`/`java/lang/Throwable`) only to
  admit the no-arg / single-`String` constructor shapes; the *type* comes from the classpath.
- ✅ **Fixed the type-alias expansion bug.** Classpath-seeded aliases carry a JVM **internal** target
  (`java/lang/Exception`, with `/`); the expansion loop only handled simple/primitive/dotted targets,
  so scanned aliases never reached `class_names`. Added the `/`-internal branch — now `class MyEx :
  Exception(m)` emits `extends java/lang/Exception` (verified via `javap`), not a bare name.
- ✅ **Ported `JavaToKotlinClassMap`** (`jvm/jvm_class_map.rs`, with a source back-reference to
  `core/compiler.common.jvm/.../JavaToKotlinClassMap.kt`) — the canonical built-in mapped types
  (`Any`, `String`, `CharSequence`, `Throwable`, `Cloneable`, `Number`, `Comparable`, `Enum`,
  `Annotation`, and the collection read-only/mutable pairs `List`/`MutableList`→`java/util/List`, …).
  These are intrinsic (not stdlib `.class` files), so they seed `class_names` unconditionally. This
  resolves `class D : Comparable<D>` → `implements java/lang/Comparable` with no JDK on the classpath.
- ✅ **Reject unresolved supertypes.** A class whose base/interface supertype resolves to none of
  {user class, classpath class, alias, mapped built-in} is rejected (skipped) instead of emitting a
  bare default-package name that would `NoClassDefFound` at load.
- ✅ `SymbolTable` now carries the alias/built-in-expanded `class_names` (simple name → JVM internal
  name) as the single source of truth; `resolve.rs` consults it and defers JVM-class knowledge to
  the `jvm` module.
- ✅ **Drop-in classpath, no env hack.** Removed `KRUSTY_KOTLIN_STDLIB`. The conformance harness and
  the exception-using e2e tests locate a real kotlin-stdlib jar from the local caches
  (`tests/common::stdlib_jar`) and pass it via `-classpath`; the harness supplies it **only for
  `// WITH_STDLIB` tests**, matching the Kotlin test directive.
- ✅ **Classpath resolution is visibility-aware.** Reading the real stdlib exposed that krusty
  resolved calls to *non-public* members — multifile-facade **part** classes
  (`StringsKt__StringBuilderJVMKt`) and **private** overloads (`ConsoleKt.println(int)`, which was
  mis-indexed as an extension and shadowed a user's own `T.println()`), causing `IllegalAccessError`
  at runtime. `ClassInfo` now carries the class access flags; `index_class_bytes`,
  `resolve_java_static`, and `resolve_java_instance` require a **public method on a public class** —
  otherwise the call stays unresolved (rejected), never miscompiled.
- ✅ TDD: full suite 178 green. Box conformance with `// WITH_STDLIB` respected: **365 compiled /
  356 box()=OK / 9 FAIL**. The 9 are pre-existing miscompiles from the undocumented post-63 work
  (secondary constructors ×3, `inline class`, `sealed` delegating ctor, devirtualization, inc/dec
  with two receivers, two VerifyErrors) — orthogonal to this phase, and the next correctness target.
  This phase **fixed** the 4 `java.lang` supertype cases and all stdlib-visibility miscompiles, and
  introduced none.

## Phase 73 — Isolate JVM bytecode emission in the `jvm` module  ✅
- ✅ Dissolved the `codegen` module: `src/codegen/emit.rs` → `src/jvm/emit.rs` and
  `src/codegen/classfile.rs` → `src/jvm/classfile.rs`. All JVM-specific code (class-file read/write,
  bytecode emission, the `JavaToKotlinClassMap` port, classpath scanning) now lives under `jvm::`.
  Public paths: `krusty::jvm::emit`, `krusty::jvm::classfile`. ~25 call sites updated.
- ✅ Full suite 178 green after the move.
- ⬜ **North star (in progress):** *no non-`jvm` module should depend on `jvm` at all.* Today
  `resolve.rs` still calls into `jvm` for classpath resolution and traffics in JVM internal
  names/descriptors (the `Ty` representation is JVM-coupled). Decoupling this — a front-end type
  representation + a resolution interface the `jvm` backend implements — is the next architectural
  step.

## Phase 74 — Secondary constructors via real grammar; reject inner classes  ✅
- ✅ **Secondary constructors parse through real productions.** Replaced the `skip_balanced(LParen,
  RParen)` / `skip_balanced(LBrace, RBrace)` token-skipping with proper parsing: extracted
  `parse_param_list` (the real parameter grammar, shared with `parse_fun`) and `parse_call_arguments`
  (real argument expressions), and parse `constructor(params) : this/super(args) { body }` into a
  real `SecondaryCtor` AST node (`CtorDelegation::{None,This,Super}`). Construction-overload emission
  is the next step; until then the checker rejects a class with secondary ctors (parsed correctly,
  not skipped → no miscompile). Fixes the secondaryConstructors/sealed-delegating box FAILs.
- ✅ **`inner class` rejected** (was silently dropped → VerifyError when used): an inner class needs
  the outer-instance capture (`Test this$0` + qualified `new`) krusty doesn't model.

## Phase 75 — Kill the remaining delimiter-skipping hacks  ✅
- ✅ **`skip_type_args` → `parse_type_args`:** generic type-argument lists `< (out|in)? type | * ,+ >`
  now parse through the real grammar, recursing via `parse_type` (so `Map<K, List<V>>` parses
  correctly). Arguments are JVM-erased, so callers discard them — but parsing is real.
- ✅ **`skip_nested_decl_body` → `parse_nested_type_decl`:** nested `class`/`object`/`interface`/
  `data|enum|annotation class`/`sealed …` parse through the real per-kind parsers (recursively) and
  are discarded (nested types still unsupported → a reference fails to resolve, never miscompiled).
- ✅ **Annotation arguments** parse through a real `parse_annotation_args`/`parse_annotation_value`
  (named args, array literals `[…]`, nested `@Anno`, and expression values incl. `Foo::class`),
  replacing the balanced-`)` token skip.
- ✅ **Enum-body** nested types / secondary ctors and the **`skip_balanced`/`skip_balanced_braces`**
  helpers removed entirely — no depth-counting delimiter skips remain in the parser.
- ✅ Full suite 178 green. Box conformance **350 OK / 4 FAIL** (FAIL 9→4: the secondary-ctor and
  `inner class` cases now reject cleanly instead of miscompiling; OK 356→350 as a few annotation/
  nested-heavy tests that the old lenient skip tolerated now reject). Remaining 4 FAIL are unrelated
  pre-existing miscompiles (devirtualization, inc/dec-two-receivers, two VerifyErrors).

## Phase 76 — Diverging property initializers + `TODO()` → `NotImplementedError`  ✅
- ✅ **`expr_diverges` now recognises any `Nothing`-typed expression** (`TODO()`, `error(…)`, a call
  to a `Nothing`-returning function, `x!!` on null), not just literal `throw`/`if`/`when`/`try`. A
  property initializer `val x: String = TODO()` is diverging, so the constructor no longer emits the
  dead `astore`/`putfield`/`return` after the throw — which had left an unreachable offset with an
  inconsistent `StackMapTable` (`VerifyError: Expecting a stack map frame`).
- ✅ **`TODO()` throws the real `kotlin.NotImplementedError`** (was a `java.lang.RuntimeException`
  stand-in), resolved from the stdlib on the classpath; the checker rejects `TODO` when
  `NotImplementedError` isn't resolvable (no stdlib) rather than emit a `NoClassDefFound`.
- ✅ TDD: `tests/diverging_init_e2e.rs` (`val x: String = TODO()` in a class, caught as
  `NotImplementedError`, on the JVM). Full suite 179 green. Fixes the `unreachableUninitializedProperty`
  box FAIL.

## Phase 77 — `++`/`--` as real AST nodes (not desugared)  ✅
- ✅ `++`/`--` no longer desugar to `name = name + 1` in the parser (which threw away structure and
  miscompiled `String++` as `"s" + 1` concat). They parse to a real `Stmt::IncDec { name, dec }`
  node — `inc`/`dec` are overloadable operators, so the resolution belongs after parsing.
- ✅ The checker resolves the target: a mutable **numeric** variable (local / top-level / class
  member) uses the built-in inc/dec; a non-numeric target would need a user `inc`/`dec` operator
  krusty doesn't model → rejected (fixes the `incDecWith2Receivers` box FAIL, `operator fun
  String.inc()`). Codegen emits `iinc` for an `Int` local, else load/±1/store (with `i2b`/`i2s`
  narrowing), for locals, top-level `var` props (`getstatic`/`putstatic`), and `this` members
  (getter/setter or field).
- ✅ TDD: full suite 179 green; existing `inc_dec_e2e` still passes.

## Phase 78 — Interface default-method return types + checker/emit type-resolution consistency  ✅
- ✅ **Interface default methods infer their return type.** `interface I { fun foo() = 42 }` was
  emitted as `void foo()` (the AST has no explicit return type → defaulted to `Unit`), so the `()I`
  call site `i.foo()` hit `NoSuchMethodError`. Emit now takes the return type from the **collected
  signature** (which applied body inference) → `int foo()`. Fixes the `kt67218i` box FAIL.
- ✅ **Checker and emit resolve the same type universe.** The checker's `resolve_ty` and emit's
  `resolve_ty` only consulted user classes, so a built-in mapped / classpath / alias type (`Number`,
  `Comparable`, `List`, …) degraded to `Ty::Error` (checker, lenient) or `java/lang/Object` (emit) —
  an inconsistency that miscompiled `x is Number` to `instanceof java/lang/Object` (always true) and
  let `Number = 0.0` through to a `VerifyError`. Both now fall back to the alias/built-in-expanded
  `class_names` (handling the `__ty/<Prim>` alias encoding), so `is`/`as`/descriptors use the real
  JVM class and primitive-to-reference assignments (which need boxing krusty doesn't do) are rejected.
  Fixes the `kt16581` box FAIL and the latent `is Number` miscompile Phase 27 had guarded by rejection.
- ✅ TDD: full suite 179 green; `is Number` runs correctly (`instanceof java/lang/Number`);
  `is_as_e2e` updated (unresolved-target case uses a genuinely-unknown type).
- ✅ **Milestone: box conformance 351 OK / 0 FAIL** — the never-miscompile invariant holds across all
  10,009 cases (down from 11 FAIL at the start of this protocol stretch). krusty is correct on 100%
  of what it accepts; remaining growth is coverage (the big subsystems: lambdas/HOF, collections,
  real generics), not correctness.

## Phase 79 — Autoboxing (primitive ↔ boxed reference)  ✅
- ✅ A primitive flowing to `Any`/`Object` (or an erased generic parameter) **boxes** to its wrapper
  (`Integer.valueOf`, `Double.valueOf`, …); a reference flowing to a primitive **unboxes**
  (checkcast + `intValue()`, …). Implemented purely at the **emit coercion site** (`emit_expr_as` +
  `box_wrapper`) — the *representation* (primitive vs boxed) is a backend concern.
- ✅ **Layering fix (per maintainer):** the checker no longer reasons about primitive-vs-boxed. Its
  `expect_assignable` expresses pure Kotlin subtyping — every type is a subtype of `Any`/`Object`,
  and the top type narrows back by an unchecked cast — with **no `is_primitive` in the front end**.
  (The real root cause, `Ty` conflating the Kotlin type with its JVM representation, is the
  multiplatform-backend refactor below.)
- ✅ **Frame-spill fixes** the boxing exposed: when a call/constructor **argument branches**
  (`if`/`when`/`try` → StackMapTable frames), the receiver / `new`+`dup` already on the stack aren't
  recorded by those frames → `VerifyError`. `emit_fun_invoke` (FunctionN) and krusty-class
  construction now spill args (and the receiver) to locals first, evaluate the branchy arg on an
  empty stack, then reload — a general latent codegen bug, now fixed.
- ✅ TDD: `tests/boxing_e2e.rs` (Int/Double/Char box+unbox round-trip on the JVM). Full suite 180
  green. **Box conformance 367 OK / 0 FAIL** (+16 from boxing; invariant held).

## Phase 80 — Front-end/back-end boundary  ✅
- ✅ `docs/ARCHITECTURE.md` + a `Backend` trait: the front end is backend-agnostic; each target is a
  pluggable backend (JVM today, WASM/JS future). The common `backend::compile` orchestrator runs the
  front-end type-check per file then hands the **checked** output to the backend's `lower_file`/
  `finalize` — `check_file` no longer lives inside the backend. Driver (`main.rs`) is a thin wrapper.

## Phase 81 — Common IR scaffold (`krusty-ir`, modeled on Kotlin IR)  ✅
- ✅ `src/ir.rs`: a **backend-agnostic, typed, index-based** IR — `IrType` (classes by Kotlin FqName,
  not JVM descriptors), `IrFunction`/`IrClass`/`IrFile`, and `IrExpr` (`Const`/`GetValue`/`SetValue`/
  `Call`/`Return`/`Block`/`When`/`TypeOp`/`While`/`Variable`) with `IrTypeOp` including an explicit
  `ImplicitCoercion` (so box/unbox/erasure are visible IR nodes, decided by backend lowering — not
  hidden in codegen). Taxonomy mirrors Kotlin IR ("don't reinvent the wheel"); deliberately **not**
  LLVM/MLIR (those are low-level/native and have no managed-VM JVM/JS path — see ARCHITECTURE.md).
- ✅ Smoke test builds a trivial `fun answer(): Int = 42` IR by hand and checks the return type is the
  Kotlin FqName `kotlin/Int`. Full suite green.
- ⬜ **Next:** `ast → ir` lowering (where the parser-rejected desugarings — `when`/`for`/`++` — belong
  as IR passes), then rewire the JVM backend to consume IR instead of the AST directly; gated by the
  conformance harness at `0 FAIL`.

## Phase 82 — `Ty::Fun` carries parameter/return types (typed function variables)  ✅
- ✅ **`Ty::Fun(u8)` → `Ty::Fun(&'static FnSig { params, ret })`** (interned, keeping `Ty` `Copy`, like
  `Ty::Array`). 35 sites across `types`/`resolve`/`emit` updated. The front end now keeps the real
  function-type signature; the JVM backend still lowers to `FunctionN` (arity).
- ✅ End-to-end typed function variables: `val f: (Int) -> Int = { it * 2 }; f(3)`. The lambda checks
  against the annotation's param types (`it`/`x` typed `Int`); a `Fun`-typed call recovers the real
  **return type** (was erased `Object`); `emit_fun_invoke` **unboxes/casts** the `Object` invoke
  result to that return type. Works for primitive and reference returns and HOF arguments.
- ✅ Function-type **assignability is by arity** (param/ret variance handled by erasure/boxing) so the
  stricter `FnSig` equality doesn't over-reject.
- ✅ TDD: `tests/fun_type_e2e.rs` (typed vars, explicit params, reference return, HOF arg on the JVM).
  Full suite 182 green. **Box conformance 367 OK / 0 FAIL** — invariant held across the type-model
  change. Foundation for general lambdas / higher-order functions.

## Phase 83 — Typed lambda parameters `{ x: Int -> ... }`  ✅
- ✅ `parse_lambda` now accepts a typed single parameter `{ x: Type -> body }` (the type is parsed
  and discarded; the parameter's type comes from the declared function type via
  `check_lambda_with_types`, Phase 82). Was a parse error ("expected an expression").
- ✅ Full suite 182 green. Box conformance **369 OK / 0 FAIL** (+2).

## Phase 84 — Member methods with function-type parameters (HOF methods)  ✅
- ✅ Class/companion method signatures now compute `lambda_param_types` (was empty), and the instance
  method-call site types lambda arguments against the method's `lambda_param_types` (so `it`/`x`
  resolve) — mirroring the free-function HOF path. `C().call { it * 2 }` works end-to-end.
- ✅ Full suite 182 green. Box conformance **369 OK / 0 FAIL** held.

## Phase 85 — Property type inference from a function-return  ✅
- ✅ A property initializer `val v = f()` infers its type from `f`'s return type. A pre-pass collects
  top-level function return types (explicit annotations) before pass-2 property processing, so order
  doesn't matter; `infer_lit_ty` consults it (a function call) before the class-name ctor path.
- ✅ Full suite 182 green. Box conformance **370 OK / 0 FAIL** (+1).

## Phase 86 — Deferred `var` initialization (`var x: T` then `x = …`)  ✅
- ✅ A `var` with a type annotation and no initializer (`var x: Int`) synthesizes the type's default
  value (`0`/`false`/`'\0'`/`null`); a later `x = …` assigns it. Was a parse error ("expected '='").
  Restricted to `var` (a `val` deferred-init needs assign-once tracking krusty lacks → still rejected).
- ✅ Full suite 182 green. Box conformance **372 OK / 0 FAIL** (+2).

## Phase 87 — `lateinit var` local variables  ✅
- ✅ A `lateinit var x: T` local consumes the modifier; the deferred-`var` path (Phase 86) handles the
  no-initializer declaration, defaulting the slot to `null`. Was "unresolved reference: lateinit".
- ✅ Full suite 182 green. Box conformance **373 OK / 0 FAIL** (+1).

## Phase 88 — Top-level computed properties (`val g: T get() = …`)  ✅
- ✅ A top-level property with a custom getter and no initializer emits a `getG()` static method on
  the facade (no backing field, no `<clinit>`); reads of `g` route to `invokestatic getG`. `SymbolTable`
  tracks `computed_props`. Requires a type annotation (no top-level getter-return inference yet). Was
  rejected ("top-level computed properties not supported").
- ✅ Full suite 182 green. Box conformance **373 OK / 0 FAIL** held.

## Phase 89 — Top-level computed-getter return inference  ✅
- ✅ A top-level computed property without a type annotation (`val g get() = 42`) infers its type from
  the expression getter body (`infer_lit_ty`), extending Phase 88.
- ✅ Full suite 182 green. Box conformance **375 OK / 0 FAIL** (+2).

## Phase 90 — `fun interface` parsed as a real interface (partial SAM)  ✅
- ✅ `fun interface F { fun m(…): R }` now parses as a real interface (`is_fun_interface` flag), so it
  can be used like any interface (`class C : F`, override, `invokeinterface`) instead of being
  dropped as an unsupported dummy. **SAM lambda-conversion** (`F { … }` → an anonymous impl with the
  method's real signature) is deferred — it's rejected cleanly (skipped), never miscompiled.
- ✅ Full suite 182 green. Box conformance **376 OK / 0 FAIL** (+1).

## Phase 91 — Bytecode-equality verified vs the real kotlinc  ✅
- ✅ Stood up a working reference `kotlinc` from local jars (no assembled dist): a wrapper running
  `java -cp <kotlin-compiler-embeddable + stdlib + reflect + script-runtime + kotlinx-coroutines +
  trove4j + jetbrains-annotations> org.jetbrains.kotlin.cli.jvm.K2JVMCompiler -classpath <stdlib>`
  on **JDK 21** (kotlinc 2.0.21 rejects JDK 25). Recorded in `docs/DIFF_KOTLINC.md`.
- ✅ Ran the differential harnesses (`tests/diff_kotlinc.rs`, `tests/diff_class_kotlinc.rs`) with
  `KRUSTY_KOTLINC`/`KRUSTY_REF_JAVA_HOME`/`KRUSTY_KOTLIN_STDLIB`: krusty's **public ABI (javap
  signatures) and execution output MATCH kotlinc** for the free-function subset
  (arith/promotion/`if`/`&&`/concat/`String.substring`/`indexOf`) and `class Point(val x, var y)`
  (ctor + accessors + construction). First confirmed differential pass vs the real compiler.
- ⬜ Next: widen the diff harness corpus (more constructs) toward byte-exact `.class` comparison, and
  wire it into CI as the standing bytecode-equality gate.

## Phase 92 — Widen the kotlinc differential corpus  ✅
- ✅ Added `when` (subject, comma arm, else), counted `for` loop, `%`, unary `-`, `Char`, and `Long`
  comparison to `diff_kotlinc.rs`. krusty's ABI (javap) and execution output **match the real kotlinc**
  for all of them (verified with the reference kotlinc from Phase 91).

## Phase 93 — `data class` ABI verified vs kotlinc  ✅
- ✅ Added `data_class_abi_matches_kotlinc` to `diff_class_kotlinc.rs`: krusty's synthesized data-class
  public member surface (`componentN`/`copy`/`equals`/`hashCode`/`toString` + accessors) matches the
  real kotlinc's exactly for `data class P(val x: Int, val y: String)`.

## Known bytecode divergence — `object` properties  ⬜
- An `object`'s properties are emitted by krusty as **instance** fields (`private final int v`,
  `getfield`); the real kotlinc emits them as **static** fields on the singleton (`private static
  final int v`, `getstatic`). The **public ABI matches** (`INSTANCE`, `getV()`, `f()`), and behavior
  is identical, but the private backing field differs → not byte-exact. Fixing it is pervasive
  (field access + accessor bodies + init + read paths all branch on `is_object`); deferred. Verified
  via `javap` diff against kotlinc.

## Phase 94 — Primitive-array init lambda `IntArray(n) { i -> … }`  ✅
- ✅ The size constructor with an init lambda (`IntArray(n) { it * 2 }`, `CharArray(n) { … }`, …)
  types the lambda parameter (the index) as `Int` and inlines the body into a counted fill loop.
- ✅ TDD: `tests/array_init_lambda_e2e.rs` (Int/Char arrays on the JVM). Box conformance held.

## Phase 95 — Frame-safe guard: reject branchy array-init bodies  ↩︎ superseded by 96
- Interim guard (`expr_branches` rejecting branchy init bodies) — replaced by the real fix below.

## Phase 96 — Branchy array-init bodies: scope the loop temps  ✅
- ⚠️ Root cause of Phase 94's `VerifyError`: the inline fill loop's temps (the value temp **and**
  any temp a branchy body allocates, e.g. an `if`'s result slot) leaked into `self.slots` *after*
  the loop. A branchy body's result temp is written only **inside** the loop, so on the
  zero-iteration path the verifier sees that slot as `top` — but later `StackMapTable` frames
  (e.g. a subsequent `return if …`) still reported it `Integer`, hence "locals[N] top vs integer".
- 🔑 Why array-init differed from normal lambdas/functions: a normal branchy body emits
  **straight-line**, so its result-temp `istore` dominates all later code and stays consistent.
  *Inlining* the body into a loop breaks that domination — the same hazard as tailrec inlining.
- ✅ Fix (`src/jvm/emit.rs`): snapshot `next_slot` before the loop; once the array is on the
  operand stack, release every slot the loop allocated (`next_slot = base; slots.retain(< base)`)
  so no dead loop temp pollutes later frames. No checker guard — branchy bodies compile correctly.
- ✅ TDD: `tests/array_init_lambda_e2e.rs` restored to a branchy body (`if (it==1) 10 else it`),
  verified with `-Xverify:all` on the JVM. Full suite **184 green**. Box conformance **376 OK / 0 FAIL**.

## Phase 97 — JDK bootclasspath via jimage (lazy, explicit) + fallout fixes  ✅
- 🎯 Box coverage **376 → 414 OK / 0 FAIL**. Driver: JDK types (`StringBuilder`, …) couldn't
  resolve, so property inference (`val sb = StringBuilder()`) and ~40 tests were blocked.
- ✅ **No invented hardcode.** JDK types resolve from the running JDK's `lib/modules` **jimage**,
  read directly (little-endian header → location table → NUL-terminated mUTF8 strings; ref:
  `jdk.internal.jimage.BasicImageReader`). A removed earlier hack hardcoded
  `StringBuilder`/`Any` — deleted.
- ✅ **Explicit on `-classpath`, no `JAVA_HOME` magic.** New `Entry::Jimage` (a cp path named
  `modules`); the harness passes `<jdk>/lib/modules` explicitly, exactly like a jar. The classpath
  library reads no env.
- ✅ **Lazy / name-based indexing** (like kotlinc/javac): `scan_types` builds `simple → internal`
  from entry **names** (jar central directory, dir walk, jimage location table) without parsing
  class bytes; only `*TypeAliasesKt.class` is parsed (for aliases). Class bytes are read on demand
  in `find`.
- ✅ User-declared classes **shadow** classpath/JDK types of the same simple name (legal Kotlin);
  only user-vs-user duplicates are `conflicting declarations`.
- 🐞 Fallout fixed (newly-compiling tests must not miscompile):
  - `() -> Unit` lambda invoke left the erased `Object` result on the stack → `VerifyError` at the
    next branch. Now popped (Unit occupies no stack slot). (`divisionByZero.kt`)
  - A type parameter with a **primitive upper bound** (`<A : Double>`) is *specialized* by kotlinc
    (primitive/IEEE-754 `==`), not erased — krusty only erases, so it now **rejects** such
    declarations rather than miscompile. (`eqNullableDoublesWithTP.kt`)
- ⬜ Follow-up: read JDK class **bytes** from the jimage (content offset + decompress) so JDK
  members resolve lazily too — today `find` returns `None` for jimage (types resolve, members don't).

## Phase 98 — Custom property accessors + the `field` keyword  ✅
- 🎯 Box coverage **414 → 424 OK / 0 FAIL**. Custom getters/setters appear in ~500 corpus files.
- ✅ Parser: `parse_top_property` now parses a custom getter **even with an initializer**
  (`val x = e\n  get() = field…`), a custom setter (`set(v) { field = … }`), and a
  visibility-only setter (`private set`) — in either order. New `PropAccessor` in the AST.
- ✅ `field` soft keyword: a checker `field_ty` binds `field` to the backing-field type inside an
  accessor body (read and `field = …` write); a `MethodEmitter.field_backing` lowers it to
  `getfield`/`putfield` on implicit `this`.
- ✅ Emit (member properties): `bp_has_field` decides the backing field (default getter, or an
  initializer/`lateinit`); a custom getter/setter body is emitted as `getX`/`setX`, the matching
  default accessor is suppressed, and `private set` emits a private default setter.
- ✅ TDD: `tests/prop_accessors_e2e.rs` (getter over `field`, setter mutating `field`, `private
  set`) on the JVM with `-Xverify:all`.
- 🛡️ Never-miscompile guards for cases not yet emitted (→ reject/skip, not miscompile):
  - `field` referenced **inside a lambda** in an accessor (no closure capture of the field) —
    `field_ty` is cleared when checking a lambda body.
  - **Top-level** property custom accessors (the facade would use the default accessor).
  - **Companion-object** property custom accessors (emitted as the default static accessor).

## Phase 99 — Nullable primitives (`Int?`): investigated, deferred  ⏸️
- 🎯 Goal: support `Int?`/`Double?`/… (120+ corpus files). Design: a nullable primitive lowers to
  its JVM wrapper (`Int?` → `java/lang/Integer`), exactly as kotlinc — so it reuses the existing
  reference + autobox machinery. Mapping owned by the type system (`Ty::boxed`/`Ty::unboxed`),
  keeping `resolve.rs` free of JVM class names.
- ✅ Front end worked end-to-end on a JVM (`!!`→unbox, `?:`→unbox, params/returns as wrapper,
  `== null`/`!= null`, assignment-boxing): a focused e2e passed with `-Xverify:all`.
- ⚠️ Deferred: enabling it surfaced **13 box-test miscompiles** — emit sites that consume/produce a
  nullable primitive without the right box/unbox/frame handling. The never-miscompile invariant
  forced a clean revert (back to **424 OK / 0 FAIL**). The remaining emit work, by failure:
  - **string templates** — `"$x"` for `x: Int?` must box in `emit_append` (`interpolation.kt`).
  - **`===`/`!==`** identity on boxed primitives must stay reference equality, not unbox
    (`identityEqualsWithNullable/*`, `negateObjectComp{,2}`).
  - **safe calls** returning `Nothing?`/nullable (`nothingNReturningSafeCall.kt`) — frame at the
    null-branch merge.
  - **data class** components/`equals` over nullable primitives (`ieee754/dataClass.kt`).
  - a few residual frame mismatches (`kt37505.kt`).
- ➡️ Next: land it behind those fixes (audit every `emit_*` site that reads `info.ty` of a value
  that may now be a wrapper), with a box/unbox helper centralizing the coercion.

## Phase 100 — Infix function call syntax (`a foo b`)  ✅
- 🎯 Infix calls were the single biggest "expected ')'" parse blocker (~900 files): `1 shl 2`,
  `a to b`, custom `infix fun`. Now parsed as `a.foo(b)`.
- ✅ Parser: a simple identifier between two operands is an infix call, with Kotlin precedence —
  tighter than comparison (bp 7), looser than additive (bp 9), left-associative. The range words
  `until`/`downTo`/`step` and the soft keywords `is`/`as`/`in` are excluded (the `for` loop parses
  ranges specially). Guarded by `starts_expr` so it only fires when an operand follows.
- ✅ TDD: `tests/infix_call_e2e.rs` (chaining + precedence vs `+`) on the JVM.
- 🛡️ Fixed a miscompile the change *exposed* (`infixFunctionOverBuiltinMember.kt`): an explicit
  `5.rem(2)`/`5.plus(2)` on a primitive binds to the builtin operator, which beats a same-named
  user extension. krusty doesn't emit primitive operator-methods, so it now **rejects** such calls
  (skip) instead of dispatching to the shadowing extension (which returned the wrong value).
- Box conformance **424 → 425 OK / 0 FAIL** (most unblocked files still need other features;
  the parse foundation compounds as those land).

## Phase 101 — `where` generic-constraint clauses  ✅
- ✅ Parser now accepts a `where T : A, T : B` clause after a function signature (before the body)
  and after a class supertype list (before the body) — a top-level parse blocker in ~15+ corpus
  files (`fun <T> T.foo(): String where T : A, T : B`, `class D<T> : Base<T>() where …`).
- ✅ Constraints are **erased** (krusty erases type parameters to `Object`); a **primitive** bound
  is rejected, same as an inline bound (Phase 97) — kotlinc specializes it, krusty can't.
- ✅ `where` may sit on a following line; the clause is peeked (position restored if absent) so
  no-`where` declarations are unaffected. Box conformance **425 OK / 0 FAIL** (unchanged — these
  files still need generics to fully compile; the parse blocker is removed for when they do).

## Phase 102 — `Int`/`Long` bitwise & shift infix methods  ✅
- ✅ `shl` `shr` `ushr` `and` `or` `xor` `inv` on `Int`/`Long` — Kotlin's named bitwise operators
  (no operator symbol, only the infix form, so no extension-shadowing concern). Now that infix
  call syntax parses (Phase 100), these resolve to the receiver type and emit the JVM bitwise
  opcodes (`ishl`/`iand`/…, `lshl`/`land`/…); `inv` is `x xor -1`.
- ✅ New `CodeBuilder` opcodes: `ior`/`ishl`/`ishr`/`iushr` and the `Long` variants
  `land`/`lor`/`lxor`/`lshl`/`lshr`/`lushr` (shifts take an `Int` amount → stack delta −1; the
  `Long` and/or/xor pop two longs → −2).
- ✅ TDD: `tests/bitwise_e2e.rs` (every op, `Int` + `Long`) on the JVM with `-Xverify:all`.

## Phase 103 — Extension properties (`val Recv.name get() = …`)  ✅
- 🎯 Dominant cause of the "property without an initializer must be 'lateinit'" bucket (~80 of 172).
- ✅ Parser: optional receiver on a top-level property (`val [<T>] Recv[<…>][?].name`), mirroring
  extension functions; `PropDecl.receiver`. Exempt from the lateinit rule.
- ✅ Resolve: registered by `(receiver descriptor, name)` in `SymbolTable.ext_props`; `recv.name`
  reads resolve via `check_member`, `recv.name = v` writes via `Stmt::AssignMember`; accessor
  bodies type-checked with `this` = receiver.
- ✅ Emit: static `getName(Recv)` / `setName(Recv, T)` methods (receiver = slot 0, like an
  extension function); reads → `invokestatic getName`, writes → `invokestatic setName`.
- ✅ TDD: `tests/ext_prop_e2e.rs` (`String`/`Int` receivers, getter over `this`) on the JVM.
- Box conformance **426 → 431 OK / 0 FAIL**.
- ⬜ Known limit (shared with extension functions): unqualified receiver-member access in a body
  (`v` rather than `this.v`) is unresolved — use `this.`.

## Phase 104 — Unqualified receiver-member access in extension bodies  ✅
- ✅ `fun Box.f() = v` / `val Box.x get() = v` now resolve `v` as the receiver's property (i.e.
  `this.v`) — previously only `this.v` worked (sibling *method* calls already resolved via
  `this_ty`). Checker: unqualified `Name` falls back to `lookup_prop(this_ty, n)`. Emit: a new
  `ext_receiver_prop` loads `this` (slot 0) and calls the getter.
- ✅ TDD: `tests/ext_unqual_e2e.rs` (ext function + ext property using unqualified `v`) on the JVM.
- 🛡️ Fixed a latent Phase 103 bug this exposed: two extension properties erasing to the same
  `(receiver, name)` (generic overloads `C<T:Any?>.p` / `C<T:Any>.p`) emitted duplicate `getP`
  methods → `ClassFormatError`. Now rejected (skip) at registration. (`genericWithSameName.kt`)
- Box conformance **431 OK / 0 FAIL** (capability + bug-fix; the unblocked files need further
  features to fully compile).
- 🛠️ Dev workflow: iterate with **debug** builds (~1.4 s rebuild) + probes/unit; reserve the full
  `--release` box conformance for the pre-commit gate. `KRUSTY_BOX_LIMIT` samples the corpus.

## Phase 105 — Nested (non-`inner`) classes  ✅
- ✅ `class Outer { class Inner(…) { … } }` — a plain nested class is a separate JVM class
  `Outer$Inner`, used in source as `Outer.Inner(…)`. The parser hoists it to the file's top level
  (name `Outer.Inner`); `class_internal` maps `.`→`$`. `inner class` stays rejected (needs the
  captured outer instance).
- ✅ Construction/use `Outer.Inner(args)` resolves (checker) and emits (`new Outer$Inner` +
  `invokespecial <init>`) via a qualified-`Member`-callee path; methods/properties on the nested
  class work like any class.
- ✅ TDD: `tests/nested_class_e2e.rs` (two nested classes, property + method) on the JVM.
- Box conformance **431 → 433 OK / 0 FAIL**.
- Note: tooling switched to **debug** builds for the box gate — proven identical bytecode/results
  to `--release` (same emitted `.class` bytes), at a 1.4 s vs 28 s rebuild.

## Phase 106 — Real AST→IR→backend pipeline + second (JS) backend  ✅
- 🎯 Validate the front-end/back-end boundary is real, not aspirational: lower a checked AST to the
  backend-agnostic `krusty-ir`, then lower the **same** IR with **two independent backends**.
- ✅ `src/ir_lower.rs` — AST→`krusty-ir` lowering for the core subset (top-level functions:
  const/param/local, primitive arithmetic & comparison, calls, `if`/`when`, `return`, blocks,
  `val`/`var`). Outside-subset files return `None` (caller keeps the direct emitter) — the IR path
  grows one construct at a time.
- ✅ `src/jvm/ir_emit.rs` — IR→JVM bytecode (maps Kotlin FqNames → JVM descriptors *here*; the IR
  carries no descriptors). Shares `CodeBuilder`/frames with the AST emitter.
- ✅ `src/js/mod.rs` — IR→JavaScript source. **No** dependency on the JVM module; no shared
  lowering. The second backend that proves the IR is target-neutral.
- ✅ TDD: `tests/ir_pipeline_e2e.rs` lowers ONE program to IR, then runs it on **`java -Xverify:all`
  AND `node`** — both print `OK`. (`IrExpr::PrimitiveBinOp`/`IrBinOp` added for built-in ops.)
- ➡️ Next: a JS conformance run over the box corpus (IR-coverable subset) on node, respecting
  `// TARGET_BACKEND:` / `// IGNORE_BACKEND:`; grow the IR subset so the JVM path migrates onto IR.

## Phase 107 — IR intrinsics as `Call`-to-symbol (no per-intrinsic node)  ✅
- 🎯 Right model for stdlib/operator semantics: **one** `IrExpr::Call` whose `callee` is a
  [`Callee`] — `Local(FunId)` (a function in this IR) or `Intrinsic(FqName)` (a stdlib/built-in
  named by Kotlin FqName, e.g. `kotlin/String.plus`). Adding an stdlib op is *data* (a new FqName
  both backends recognize), **not** a new IR node. `PrimitiveBinOp` stays only because it's a single
  parameterized node for universal numeric/boolean ops.
- ✅ `String.plus` lowered to `Call(Intrinsic("kotlin/String.plus"))`; each backend's platform layer
  realizes it — JVM `StringBuilder().append(..).append(..).toString()`, JS `(a + b)`. Verified on
  `java -Xverify:all` AND `node`, including the chain `"a"+"b"+"c"+2+"d"` → `"abc2d"`.
- ✅ JS box conformance **parallelized** (rayon pool, big worker stacks): full corpus scan in
  **~1.5 s** (was minutes). 5 IR-lowered files, 5 OK, 0 FAIL. The JVM box harness was already
  parallel (rayon, persistent JVM per thread).
- Note: chained `+` lowers to nested `String.plus` (runtime-correct); kotlinc flattens to one
  `StringBuilder` — a future bytecode-equality optimization, not a correctness gap.

## Phase 108 — String templates in the IR  ✅
- ✅ `ir_lower` lowers `Expr::Template` (`"a${x}b"`) to a fold of `Call(Intrinsic("kotlin/String.plus"))`
  — no new node, reusing the intrinsic-symbol design from Phase 107. Each backend realizes the
  concatenation + to-string from its stdlib (JVM `StringBuilder`/`append`, JS `+`).
- ✅ Verified on `java -Xverify:all` AND `node` (`"v=$s!"` → `"v=5!"`). JS box conformance grows
  **5 → 7 IR-lowered, 7 OK, 0 FAIL** (templates are pervasive in `box()` results).
- Each construct added to `ir_lower` widens the IR path on *both* backends at once — the mechanism
  for eventually moving the JVM path off `emit.rs` onto the IR.

## Phase 109 — `while` loops in the IR  ✅
- ✅ `ir_lower` lowers `Stmt::While` to `IrExpr::While`; the JVM backend emits the counted
  back-edge with `StackMapTable` frames, the JS backend a `while (..) { .. }`. Verified on
  `java -Xverify:all` AND `node` (`sumTo(4) == 10`). 193 unit tests green, JS box 7/7, 0 FAIL.

## Phase 110 — Classes in the IR (both backends)  ✅
- ✅ The IR now models user types: `IrClass` (fields + instance methods), and the nodes
  `GetField`/`New`/`MethodCall` (structural, not per-intrinsic). `ir_lower` lowers a *simple* class
  (primary ctor of `val`/`var` props, expr-body instance methods, no inheritance/body-props) plus
  construction, field read (`this.x`/unqualified/`p.x`), and method calls.
- ✅ JVM backend emits a `.class` per `IrClass` (public fields, `<init>` storing each, instance
  methods with `this` in slot 0) via `emit_all`; JS backend emits a `class { constructor; methods }`
  with `this`. Same IR, both targets.
- ✅ TDD: `tests/ir_pipeline_e2e.rs` constructs `Point(3,4)`, reads `p.x`, calls `p.sum()`/`p.shifted(10)`
  — on `java -Xverify:all` AND `node`. JS box conformance **7 → 12 IR-lowered / 12 OK / 0 FAIL**.
- 🐞 Fixed an IR-emit frame bug: a local's slot was claimed in frames recorded *inside* its branchy
  initializer (verifier saw `top`); now the slot is allocated after the initializer is emitted.

## Phase 111 — `for` range loops in the IR  ✅
- ✅ `ir_lower` desugars `for (i in a..b [step s])` / `until` / `downTo` over `Int` to the existing
  `IrExpr::While` (bound hoisted to a local, evaluated once; step defaults to 1; `downTo` counts
  down). No new node — reuses `While`/`Variable`/`SetValue`/`PrimitiveBinOp`.
- ✅ Verified on `java -Xverify:all` AND `node` (`1..4` → 10, `0 until 3` → 3). JS box conformance
  **12 → 13 IR-lowered / 13 OK / 0 FAIL**. 193 unit tests green.

## Phase 112 — `when` (subject) + unary ops in the IR  ✅
- ✅ `when` is just if/elseif/else — it lowers to the same `IrExpr::When` (branches of
  `(condition → result)`, `else` = `None` condition). With a subject, each branch condition becomes
  `subject == arm_value` (OR-ed for multi-value arms like `1, 2 ->`). No separate node from `if`.
- ✅ Unary: `-x` → `0 - x` (typed zero); `!x` → `x == false` — reusing `PrimitiveBinOp`, no unary node.
- ✅ Verified on `java -Xverify:all` AND `node` (`when (n) { 0->; 1,2->; else-> }`, `-5`, `!(a>0)`).
  JS box conformance **13 → 17 IR-lowered / 17 OK / 0 FAIL**. 193 unit tests green.

## Phase 113 — Double/Float/Char primitives in the IR  ✅
- ✅ `ir_lower` lowers `Double`/`Float`/`Char` literals; the JVM backend emits the native
  instructions (`dadd`/`fadd`/…, `dcmpg`/`fcmpg` for compares, `push_double`/`push_float`), the JS
  backend numeric literals (`Char` as a 1-char string). Verified on `java -Xverify:all` AND `node`
  (`2.5 * 4.0 + 1.0`, `1.5f + 0.5f`, `'q' == 'q'`).
- JS box conformance steady at 17/17, 0 FAIL (these box tests need more stdlib to lower); the IR's
  numeric breadth grows with no regression. 193 unit tests green.

## Phase 114 — `toString()` / `String.length` stdlib intrinsics  ✅
- ✅ `x.toString()` → `Call(Intrinsic("kotlin/Any.toString"))`; `s.length` →
  `Call(Intrinsic("kotlin/String.length"))` — backend-mapped, no new IR nodes. JVM:
  `String.valueOf(<overload>)` / `String.length()`; JS: `String(x)` / `x.length`.
- ✅ Verified on `java -Xverify:all` AND `node` (`42.toString()`, `"hello".length`,
  `true.toString()`). JS box conformance steady 17/17, 0 FAIL (these files need more features to
  fully lower); each intrinsic is one symbol the backends map. 193 unit tests green.

## Phase 115 — IR→JVM conformance on the real corpus (+ statement-`when`/Unit fixes)  ✅
- ✅ New harness `tests/kotlin_box_ir_jvm_conformance.rs`: for each JVM-applicable box test in the
  IR core subset, lower AST→`krusty-ir`→**`ir_emit`** (NOT the AST emitter) and run on a real JVM.
  This measures the IR pipeline's *JVM* coverage of the actual corpus — the precursor to routing
  the JVM box path through `ir_emit` and retiring `emit.rs`. Result: **20 lowered / 20 OK / 0 FAIL**
  (JS path: 17/17). Respects `TARGET_BACKEND`/`IGNORE_BACKEND`; parallel (rayon, big stacks).
- 🐞 Fixes the corpus surfaced (the e2e hadn't): (a) a Unit function's trailing expression was
  lowered but dropped — now run for effect; (b) a no-`else` `when` is a Unit *statement* — emitted
  for effect, not as a value; (c) the resulting double `return` (explicit + `emit_method` fallback)
  left a frameless dead instruction → keep only the backend's single trailing `return`.
- ℹ️ `if` and `when` remain ONE IR node (`IrExpr::When`); `emit_when` is just the backend codegen
  for that node (both lower to it). Unsigned-type files are skipped (krusty has no unsigned model).

## Phase 116 — Arrays as a regular type + intrinsic ops (both backends)  ✅
- ✅ Arrays are **not** special IR nodes nor a special `IrType` — they are a regular class type
  (`kotlin/IntArray`, `kotlin/Array<T>`, like `List`) the backend lowers, and their operations are
  ordinary `Call`-to-intrinsic: `IntArray(n)` → `kotlin/IntArray.<init>`, `a[i]` → `kotlin/Array.get`,
  `a[i] = v` → `kotlin/Array.set`, `a.size` → `kotlin/Array.size`. The element type is read from the
  receiver's type (or the per-element ctor name). No node-per-operation.
- ✅ JVM backend lowers to native array instructions (`newarray`/`Xaload`/`Xastore`/`arraylength`,
  array verif types); **JS backend lowers primitive arrays to typed arrays** (`IntArray` →
  `Int32Array`, `DoubleArray` → `Float64Array`, …) — the real Kotlin/JS representation (the full
  platform answer is `kotlin-stdlib-js`'s array types).
- ✅ Verified on `java -Xverify:all` AND `node` (fill, index get/set, `.size`, `for` over `0 until
  a.size`). IR→JVM corpus conformance **20 → 21 / 0 FAIL**; JS **17 → 18 / 0 FAIL**. 194 unit tests.

## Phase 117 — `Callee::External` (stdlib = expect/actual, not per-op intrinsics)  ✅
- ✅ Renamed `Callee::Intrinsic` → **`Callee::External`**: a `Call` to any symbol *not defined in this
  IR file* (a stdlib `expect`/operator by Kotlin FqName). The IR carries only the FqName and decides
  nothing; the **backend** resolves it the way kotlinc does — (1) if in the small **intrinsic table**
  (array access, arithmetic, `String.length`/`get`, …) emit target bytecode; (2) else resolve the
  platform **`actual`** from the linked stdlib (`kotlin-stdlib-jvm`/`-js`) and emit a normal call.
  So stdlib is **not** "all intrinsics" — only the ~50 kotlinc itself intrinsifies; the rest are
  `expect`/`actual` library calls. No per-op IR node, no array/string-special types.
- ✅ Added `String.get` (`s[i]` → `Char`): JVM `String.charAt`, JS `s[i]`; distinct from `Array.get`.
- ✅ Verified on `java -Xverify:all` AND `node`. IR→JVM corpus **21/21**, JS **18/18**, 0 FAIL.
  194 unit tests green.
- ⬜ Next: wire the **linked-`actual`** path (resolve a non-intrinsic External's owner/descriptor
  from the platform stdlib and emit a normal call) so WITH_STDLIB box tests lower without per-fn code.

## Phase 118 — `is`/`as` + autobox coercion via `TypeOp` (both backends)  ✅
- ✅ `x is T`/`x !is T`/`x as T` lower to the **existing** `IrExpr::TypeOp` (no new node — a new AST
  construct collapsed onto a node already in the IR). `TypeOp` is value⊗*type* (its 2nd operand is an
  `IrType`, not an expr) — categorically distinct from `PrimitiveBinOp` (value⊗value), exactly as
  Kotlin IR separates `IrTypeOperatorCall`. JVM: `instanceof`/`checkcast`; JS: `instanceof` /
  `typeof === "string"` (cast is a no-op in untyped JS).
- ✅ Autoboxing made **explicit in the IR**: a primitive arg into a reference param (`describe(7)`
  where param is `Any`) lowers to `TypeOp(ImplicitCoercion)`; the backend boxes (`Integer.valueOf`)
  / unboxes — visible in the IR, not hidden in codegen. Drove `describe(Box)`/`("hi")`/`(7)` correct
  on `java -Xverify:all` AND `node`.
- ✅ Added a blockers diagnostic (`tests/ir_blockers.rs`): of 393 parse+check-OK non-lowered JVM box
  files, the top real blockers are lambdas (101), WITH_STDLIB (104), is/as (86), inheritance (79),
  generics (61), nullable (54) — guiding what to collapse next. Conformance holds (all-or-nothing per
  file: these files also need other features). 195 unit tests green, IR→JVM 21, JS 18, 0 FAIL.

- ✅ Member write `obj.f = v` lowers to the new `IrExpr::SetField` (mirroring the existing
  `GetField`/`SetValue` pair — read+write symmetry, not a new family of nodes). JVM `putfield`,
  JS `recv.f = v`; verified `c.n = 5; c.n = c.n + 3` → `"OK"` on `java` and `node`.
- ✅ Box-test **classpath former is directive-aware and self-provisioning** (`tests/common`):
  `WITH_STDLIB`/`WITH_RUNTIME` add kotlin-stdlib + kotlin-test + annotations; `WITH_REFLECT` reflect;
  `STDLIB_JDK8` stdlib-jdk8; `WITH_COROUTINES` coroutines — mirroring kotlinc's
  `JvmEnvironmentConfigurator`. Jars are resolved **dist-first** (the exact `lib/` of the kotlinc we
  differential-test against, via `KRUSTY_KOTLINC`), then **downloaded from Maven Central** into
  `~/.cache/krusty-deps` if absent — so `kotlin.test.*` assertions actually resolve+run instead of
  silently skipping. `tests/dep_resolution.rs` proves it.

- ✅ Block-body methods (`fun m(): R { … }`) join expr-body methods in the class subset — they route
  through the **same `lower_body`/`block_as_body`** as block-body top-level funs (a block-body method
  is no different from a block-body top-level fun), so `is_simple_class` no longer rejects them. e2e:
  a `while`-loop method runs `OK` on `java -Xverify:all` and `node`. `ir_blockers` also reworked to
  rank **decl-level** blockers — the 267-file "no unsupported expr" bucket breaks down as: body
  properties 59, init block 58, top-level property 46, base class 44, block-body method 41, enum 37,
  open 37, interface 29, supertypes 25, data 16 — guiding what to collapse next.

- ✅ **Class-body properties + `init {}` blocks** in the IR class subset (the fattest decl-level
  bucket — 59+58 near-miss files). `IrClass` gains `ctor_param_count` (the leading fields that are
  constructor parameters) and `init_body` (an effect `Block` run in the constructor after the params
  are stored). Lowering: body-prop fields append after ctor-param fields; `init_order` lowers to
  `SetField`s (property initializers) + lowered `init` blocks, with `this`=value 0 and the ctor
  params as values 1..=N. Unqualified writes to a `var` field (`total = …` in an `init`/method) now
  fall back to `SetField` like the read path. `ty_of` resolves user-class names to their internal
  type (was `Error` → bad descriptor). JVM: ctor descriptor uses only the param fields, `New` too;
  the constructor emits `init_body`. JS: constructor params are `v1..=vN`, then `init_body`. Also:
  Kotlin `==`/`!=` on **reference** operands emits `Objects.equals` (was `if_icmp*` → `VerifyError`
  on `Object`). IR→JVM corpus **31/31 run-verified OK, 0 FAIL** (was 21); JS 26 OK; lower count
  22→32. e2e: a `Counter` with a body-prop initializer + `init` block runs `OK` on java and node.

- ✅ **Top-level (module) properties** in the IR. New IR concept `IrStatic` (`IrFile.statics`) plus
  `IrExpr::GetStatic`/`SetStatic` — a top-level `val`/`var` is a `public static` field on the file
  facade, initialized in `<clinit>` in declaration order; reads/writes are `getstatic`/`putstatic`
  (JVM) or a module-level `let`/assignment (JS). Unqualified name resolution gained a statics tier
  between locals and `this`-fields. Also hardened `lower_arg`: a primitive→different-primitive
  coercion (`Int` → `Long`, not yet modeled) now **bails** so the file falls back to the direct
  emitter instead of miscompiling. IR→JVM corpus **34/34 run-verified OK, 0 FAIL**; JS 29 OK; lower
  count 32→35. e2e: a top-level `val` + mutated `var` run `OK` on java and node.

- ✅ **Classpath `scan_types` is process-globally memoized** (keyed by the entry path set). The JDK
  jimage (`java.base`) and stdlib jars are identical across every compiled file, but the harness
  builds a fresh `Classpath` per file, so the whole-JDK scan ran ~80× (~2.5 s each → it dominated
  wall time). Now the first file pays, the rest reuse. Box suite: **1500 files 75 s → 12.6 s** (sigs
  thread-sum 199 s → 7.4 s); the **full 10 009-file corpus now runs in 59 s** (was impractical),
  re-establishing the production drop-in baseline: **431 box()=OK, 0 FAIL** (~4.3% — the direct
  emitter never miscompiles, it is just narrow). This is the metric the drop-in goal is measured by;
  the IR path (34/34) is the future production backend catching up to it.

- ✅ **Reference-compiler correction.** The corpus (`~/external-projects/kotlin`) was switched to the
  **2.4.0** release branch and the differential oracle to **kotlinc 2.4.0** (downloaded; runs on Java
  25). The previous `/tmp/kdist` kotlinc was **1.9.24** — wrong version vs the corpus AND it crashes
  on Java 25 (`IllegalArgumentException: 25.0.3`). Re-baselined the production drop-in path on the
  2.4.0 corpus: **438 box()=OK / 7352 scanned, 0 FAIL**.
- ◐ **Value/inline classes — groundwork only.** Added `ClassDecl.is_value` and parser plumbing for
  `@JvmInline value class`; the parser no longer hard-errors. But compiling a value class as an
  ordinary final class is **unsound** — verified 2 box FAILs (inline-class equality
  `NZ2(NZ1(null))` and an unbox/cast `C("OK").foo`). True support needs kotlinc's unboxed
  `box-impl`/`unbox-impl`/`constructor-impl`/`equals-impl0` members **plus use-site name mangling**
  (a function taking a value class takes the underlying type under a `name-<hash>` symbol). Until that
  lands, `is_value` skips cleanly at resolve, preserving the **0-FAIL** invariant. Full `Some` spec
  captured from kotlinc 2.4.0 for the real implementation.

- ✅ **Instantiable annotations — implemented** (the literal first failing single-file box test,
  `annotations/instances/annotationAnnotationParam.kt`, now passes). An `annotation class A(val t: T)`
  emits as an interface `extends java/lang/annotation/Annotation` with an accessor per member; an
  instance `A("a")` constructs a synthetic `<facade>$annotationImpl$A$0` class (emitted once per type)
  implementing the interface with JLS member-wise `equals`/`hashCode` (`Σ 127·name.hashCode() ^
  value.hashCode()`, arrays via `Arrays.equals`/`hashCode`), `toString`, and `annotationType()`.
  Member reads `a.x` lower to `invokeinterface A.x()`; `hashCode`/`equals`/`toString` on an annotation
  receiver dispatch virtually. Both narrowly scoped to annotation receivers so null-safe paths
  elsewhere are untouched. Arrays-of-reference + nested annotations supported; array-of-primitive
  members skip. Production drop-in: **438 → 442 box()=OK, still 0 FAIL**.
- ◐ **Instantiable annotations — earlier groundwork** (the literal first failing single-file box test,
  `annotations/instances/annotationAnnotationParam.kt`: `A("a")` constructs an annotation instance
  with JLS member-wise equality). kotlinc 2.4.0 emits the annotation as an interface extending
  `java/lang/annotation/Annotation` plus a synthetic `<facade>$annotationImpl$A$0` class with
  `equals`/`hashCode` (JLS: `Σ 127·name.hashCode() ^ value.hashCode()`), `toString` (`@A(t=…)`),
  and `annotationType()` — full bytecode captured. Added `ClassDecl.is_annotation` + parser keeps the
  decl (was silently dropped). Emitting it as a plain class gives identity equals (a FAIL), so it
  skips at resolve until the impl-class synthesis (incl. `Array`/nested members) lands — preserving
  the **0-FAIL** invariant. This and value classes are each a large, intricate, byte-exact codegen
  phase; the corpus's alphabetically-first `annotations/` dir front-loads exactly these.

- ✅ **`Object` methods on any reference type** (`hashCode`/`equals`/`toString` on user classes,
  data classes, `Any`, etc.) — resolve + emit via virtual dispatch (so overrides still win). Fixed
  two latent bugs this exposed: data-class member `hashCode` is now null-safe (`Objects.hashCode`,
  was NPE on a null member — `genericNull`), and `toString` lowers through `String.valueOf` to match
  Kotlin's null-safe `toString` (`null.toString() == "null"` — `noCoercion…`). Function/lambda
  receivers are excluded (their `hashCode` identity needs lambda-singleton codegen, not yet done).
  Production drop-in: **442 → 455 box()=OK, 0 FAIL**.

- ✅ **Multi-parameter lambdas** (`{ a, b -> … }`). The AST lambda became `params: Vec<String>`
  (was a single `Option<String>`); the parser detects a param list by scanning for a top-level `->`
  before the lambda's `}` and parses a comma-separated list; the resolver binds each param; the
  emitter's `FunctionN` codegen (already arity-generic) binds params to slots `1..=N`. Verified a
  2-arg `{ x, y -> x + y }` runs `OK`. This is the **prerequisite for callable references** (e.g.
  `Any::equals` is a 2-arg function). Production drop-in: **455 → 457 box()=OK, 0 FAIL**.

- ✅ **Capturing lambdas.** A lambda that reads an enclosing local now captures it: the `$lambda$N`
  class gets a private field per captured var, `<init>(captures)` stores them, the `invoke` prologue
  copies each field into a local (so the body emits unchanged), and the call site passes the captured
  values. Captures are detected as outer-slot names the body references (minus the lambda's own
  params). Verified `{ x -> x + base }` capturing `base` runs `OK`. A lambda that calls a local
  function is rejected (the recursive nested-closure dispatch isn't modeled — preserves 0-FAIL). Last
  prerequisite for **callable references**. Production drop-in: **457 → 458 box()=OK, 0 FAIL**.

- ✅ **Callable references (Object methods)** — `Any::equals`/`obj::hashCode`/`obj::toString`, the
  `annotationAnyDispatch` first-failing test. A receiver that names a value is *bound* (captures it,
  arity = method args); one that names a type is *unbound* (the receiver becomes the first param).
  Emit generates a `FunctionN` whose `invoke` performs the method on its target and boxes the result.
  Other callable references still skip. Completes the multi-param → capturing → callable-ref chain.
  Production drop-in: **458 → 460 box()=OK, 0 FAIL**.

- ✅ **Class literals + `KClass` members** (`annotationEqHc` test). `UserType::class` lowers to
  `ldc UserType.class` (modeled as `java.lang.Class`); `KClass<*>` resolves to `java.lang.Class` in
  both type resolvers (checker + emitter — a mismatch there caused a `NoSuchMethodError`). Restricted
  to declared-class receivers — primitive `Int::class` (needs `Integer.TYPE`) and bound `obj::class`
  (needs `getClass()`) skip rather than emit a bad `ldc` (caught 8 FAILs incl. lateinit tests using
  those forms). Also fixed annotation equality for `Float`/`Double` members to JLS boxed semantics
  via `Float.compare`/`Double.compare` (`NaN==NaN`), where `fcmpg`/`dcmpg` gave `NaN!=NaN`.
  Production drop-in: **460 → 463 box()=OK, 0 FAIL**.

- ✅ **Constructor default arguments.** `ClassSig` gains `ctor_defaults` (the default `ExprId` per
  primary-ctor param; box tests are single-file so the ids are valid at the call site). A `Name(...)`
  constructor call may omit trailing args whose params have defaults; the emitter fills each omitted
  param with its default expression. Restricted (to hold 0-FAIL) to **simple-literal defaults whose
  literal kind matches the param's primitive category** — adapting defaults (`Long = 0`) and complex
  ones (anonymous objects, `emptyArray()`) still skip. Also fixed a real crash: `copy$default`'s mask
  `1 << i` panicked for a >32-field data class (now `wrapping_shl`). Production drop-in: **463 → 468
  box()=OK, 0 FAIL**.

- ✅ **Stdlib-annotation instantiation** (`annotationFromStdlib`): `kotlin.SinceKotlin("1.6.0")`.
  A qualified-name callee (`Member(Name("kotlin"),"SinceKotlin")`) is recognized as a **classpath**
  annotation: its members are read from `Classpath::find("kotlin/SinceKotlin").methods` (no-arg
  accessors → `desc_to_ty`), and the same `$annotationImpl$` synthesis is emitted against the existing
  stdlib interface (not re-emitted). `toString` yields the FQ `@kotlin.SinceKotlin(version=1.6.0)`.
  New shared helpers `qualified_path` + `classpath_annotation_members`. Production drop-in: **468 →
  469 box()=OK, 0 FAIL**. (Concludes the `annotations/instances/` high-value cluster — remaining tests
  there are narrow niches; the next big leverage is corpus-wide: inheritance, generics, enums, etc.)

- ✅ **`emptyArray()`** (a common corpus-wide stdlib intrinsic). Typed as `Array<Null>` (a bottom
  array) — assignable to any reference array in `expect_assignable` — and **materialized with the
  target element type** in `emit_expr_as` (`val a: Array<String> = emptyArray()` → `new String[0]`,
  so the descriptor matches and there's no `VerifyError`). A no-target use falls back to `Object[0]`.
  This is krusty's first bit of **expected-type-directed codegen** for a general call. Production
  drop-in: **469 → 471 box()=OK, 0 FAIL**.

- ✅ **Array-literal `[...]` syntax** (Kotlin's collection-literal form, used in annotation
  arguments/defaults). The parser desugars a primary-position `[a, b]` → `arrayOf(a, b)` and `[]` →
  `emptyArray()`, reusing the array-builtin resolution + target-typed codegen. Index access `a[i]`
  (postfix) is unaffected. Verified `val a: Array<String> = ["x","y"]` / `[]` runs `OK`. **+0 box**
  (the corpus tests using `[...]` also need KClass/enum/annotation defaults + `contentEquals`), but a
  correct general feature that removes a parser blocker. Still **471 box()=OK, 0 FAIL**.

- ✅ **Top-level function references `::foo`** (chosen via a leverage map: callable refs blocked ~21
  non-annotation tests). `::foo` resolves to `Fun(params, ret)` of the function; emit synthesizes a
  captureless `FunctionN` whose `invoke` unboxes its `Object` args to the parameter types, calls
  `facade.foo(...)`, and boxes the result — reusing the `emit_callable_ref` scaffold. Production
  drop-in: **471 → 478 box()=OK, 0 FAIL** (+7). (Bound/unbound *method* refs `obj::m`/`Type::m` for
  arbitrary methods still skip — a follow-up.)

- ✅ **Reference array constructor `Array(n) { i -> e }`** (leverage map: `Array` was the top
  unresolved function, ~34 files). Resolves to `Array<elem>` where `elem` is the lambda's return
  (boxed when primitive — `Array<Int>` is `Integer[]`); the index param is typed `Int`. Emit reuses
  the existing `IntArray(n){…}` counted-fill loop (now reached via `is_array_builtin("Array")`), which
  already does `anewarray`/`aastore`/boxed-element for a reference element. A nested-array element
  (`Array(n){ DoubleArray(m) }`) is skipped (its loop-fill StackMapTable interacts badly with
  surrounding loops — caught 1 FAIL). Production drop-in: **478 → 480 box()=OK, 0 FAIL**.

- ✅ **`StringBuilder.appendLine`** (leverage map: top unresolved method, 19 files) → `append(x)` then
  `append('\n')` (it's a Kotlin extension, not a JDK method). +12 raw, but it unblocked files exposing
  **two pre-existing bugs**, both then guarded to hold 0-FAIL: (a) **nested try/catch** trips a
  StackMapTable frame bug (verified `append` in nested try/catch `VerifyError`s independent of
  `appendLine`) — rejected via a new `expr_has_try` walker; (b) a **lateinit *local*** defaults to
  `null` instead of throwing on read-before-init (miscompiles a negative test) — rejected at parse.
  Net **480 → 485 box()=OK, 0 FAIL**. (Nested-try frames + lateinit-local throw are now logged
  follow-up bugs.)

- ✅ **General method references** `obj::m` (bound, captures the receiver) / `Type::m` (unbound, the
  receiver is the first parameter), on user-class methods — extends the `FunctionN` scaffold:
  `emit_method_ref` casts the receiver to the class, unboxes args, `invokevirtual`/`invokeinterface`,
  boxes the result. Guards for the 2 exposed FAILs: an **object** receiver (`O::m`, bound to the
  singleton — not modeled) is skipped; **`suspend` functions** are now **rejected** (krusty emits no
  coroutine `Continuation` state machine, so compiling them as plain functions is unsound — this also
  fixed a callable-ref-equality FAIL). Net **485 → 491 box()=OK, 0 FAIL** (+6; suspend rejection
  dropped 2 previously-lucky unsound passes).

- ✅ **Constructor references `::ClassName`** → `Fun(ctor_params, ClassName)`; `emit_ctor_ref`
  synthesizes a captureless `FunctionN` whose `invoke` does `new ClassName` + unbox-args +
  `invokespecial <init>`. Completes the callable-reference family (top-level fun, bound/unbound
  method, constructor). Production drop-in: **491 → 493 box()=OK, 0 FAIL**.

- ✅ **Bridge methods** (the dominant leverage lever — ~83 blocked files). When a class's concrete
  override has a different erased signature than a supertype method, the checker now **records** a
  `BridgeSpec` (in `TypeInfo.bridges`) instead of rejecting; `emit_bridges` emits a synthetic
  `ACC_BRIDGE|ACC_SYNTHETIC` method with the erased descriptor that, per parameter, **checkcasts** a
  reference / **unboxes** a primitive / passes through, then `invokevirtual`s the concrete method.
  Edge cases handled to hold 0-FAIL: a bridge whose signature duplicates an existing method is skipped
  (`ClassWriter::has_method`); a **void** return uses `return` not `areturn`; a bridge is only recorded
  when each erased param is `Object` or equals the concrete (else `method_of` picked a wrong overload —
  e.g. the `format` diamond); a differing primitive return is left out. Production drop-in: **493 →
  526 box()=OK, 0 FAIL** (+33, the biggest single-phase gain).

- ✅ **`String` classpath-supertype assignability** (leverage map: "inferred String but CharSequence
  expected", 16 files). `expect_assignable` now accepts `String` where `CharSequence`/`Comparable`/
  `Serializable` is expected (krusty's `obj_is_subtype` only knew *user*-class hierarchies). One rule,
  **526 → 539 box()=OK, 0 FAIL** (+13).

- ✅ **Standalone `run { … }`** (leverage map: top unresolved function after `listOf`, ~12 files) →
  the no-param lambda body is inlined, yielding its value (resolve + emit, like the `with` scope
  function). It exposed a pre-existing **elvis-with-`Unit`-RHS** frame bug (`x ?: someUnitExpr`
  pushes incompatible stack shapes → `VerifyError`), now guarded (skip). Production drop-in: **539 →
  545 box()=OK, 0 FAIL** (+6).

- ✅ **Explicit builtin operator-methods on numeric primitives** (leverage map: "builtin operator
  method on a primitive", 26+ files, erasure-free). `a.plus(b)`/`minus`/`times`/`div`/`rem` now map
  to the same numeric promotion + bytecode as `a + b` (reusing `check_binary` / `emit_arith`);
  `a.compareTo(b)` → `Int` via `{Integer,Long,Float,Double}.compare` (IEEE-aware, so
  `0f.compareTo(-0f) == 1`); `a.unaryMinus()`/`unaryPlus()`. The resolver and `emit_call` re-derive
  from receiver type + name (no side-table). Correctness guard: krusty parses infix `a rem b` and the
  dot form `a.rem(b)` to the **same** AST, but Kotlin routes infix to a user `operator`/`infix`
  extension while the dot form keeps the builtin — so when a user extension of that name exists for
  the receiver type, krusty rejects (skip) rather than guess (caught a miscompile in
  `infixFunctionOverBuiltinMember.kt`). `mod` (floor-semantics), `rangeTo`, `inc`/`dec` stay rejected.
  Production drop-in: **545 → 557 box()=OK, 0 FAIL** (+12).

- ✅ **`Char` arithmetic** (leverage map: part of "operator cannot be applied", erasure-free).
  `check_binary` now types `Char + Int` / `Char - Int` → `Char` and `Char - Char` → `Int` (Kotlin's
  only `Char.plus`/`Char.minus` overloads — there is no `Char + Char`, `Char * …`, etc.). Codegen
  computes in `int` then truncates with the new `i2c` opcode (0x92) for a `Char` result, matching
  Kotlin's wrap-mod-2^16 (`Char.plus(Int) = (code + n).toChar()`). Production drop-in: **557 → 558
  box()=OK, 0 FAIL** (+1; most `Char`-arith files have further blockers).

- ✅ **Phase 148 — retire the direct AST emitter; IR is the sole JVM codegen path.** `src/jvm/emit.rs`
  (the 5786-line direct AST→bytecode emitter) is **physically removed**. `JvmBackend::lower_file` now
  lowers each checked file to `krusty-ir` (`ir_lower::lower_file`) and emits via `ir_emit::emit_all`.
  The two pure helpers the IR path still needs (`file_class_name`, `method_descriptor`) moved to the
  new `src/jvm/names.rs`. Consequences (accepted, intentional): JVM box coverage drops from the
  emitter's **558** to the IR subset's **37** (0 FAIL) — the IR path is far less complete, so the
  bulk of the corpus now *skips* through the backend. The 72 e2e tests that drove the removed emitter
  were deleted; the remaining CLI-driven e2e tests were made **tolerant** (skip when the IR backend
  rejects a construct, so they auto-revive as `ir_lower` grows). Fixed one IR miscompile to hold the
  0-FAIL invariant under the new backend: a constructor call with a default/secondary-ctor mismatch
  (`Foo()` on `class Foo(val box: String = "OK")`) emitted `invokespecial <init>(String)` with no
  argument on the stack (VerifyError) — `ir_lower` now bails (skips) when a `New`'s arg count ≠ the
  primary constructor's parameter count. Suite green (87 bins). KNOWN, pre-existing/unrelated:
  `diagnostics_match_kotlinc` (gated by `KRUSTY_KOTLINC`) drifts vs kotlinc 2.4.0's reworded
  diagnostics (`unresolved reference 'q'.` vs krusty's `unresolved reference: q`) — a separate
  diagnostics-wording task, not part of this migration.

### IR-migration backlog (drive the IR path back toward the emitter's coverage)
The IR backend (`ir_lower` + `ir_emit`) must regain what the direct emitter did. Highest-leverage gaps
(each a phase): top-level property **getter/setter ABI** (IR emits public static fields, not Kotlin's
private-field+accessors); **constructor default arguments**; the operator/`Char` arithmetic just added
to the AST checker (Phases 146/147 resolve typing survives, but `ir_lower`/`ir_emit` must lower it);
broad `box()` constructs (when/try/lambdas/strings) to climb from 37 back toward 558.

- ✅ **Phase 154 — `enum class` in the IR backend** (112 → 128 box()=OK, 0 FAIL).
  **`enum class`** is implemented end-to-end: `IrClass` gained a `superclass`
  (`java/lang/Enum`) and `enum_entries`; `emit_enum_class` emits the entry static-finals, a `$VALUES`
  array, a private `(String,int,…)` ctor → `super(name,ordinal)`, a `<clinit>` that builds them, and
  synthetic `values()`/`valueOf(String)`; `E.ENTRY` → `getstatic`, `e.ordinal`/`e.name` →
  `Enum.ordinal()`/`name()`, and the checker resolves `E.values()`/`E.valueOf()`. Two latent bugs
  fixed along the way: a `val x: UserType` local was typed `Error` (broke reference `==` → wrong
  primitive-compare path), and a smart-cast field receiver now gets a `checkcast`. Guards hold 0-FAIL
  on shapes the flat emitter can't do yet (skip, never miscompile): no-`else` `when` used as a value
  (exhaustiveness unproven), branchy enum-entry args (ambient-stack merge frames), enum entry bodies /
  abstract enum methods. KNOWN shortcut to generalize: `e.ordinal`/`e.name` are emitted as intrinsics
  rather than via general inherited-method resolution on the `java/lang/Enum` superclass.

- ✅ **Phase 155 — `data class` via backend-agnostic IR synthesis** (128 → 140 box()=OK, 0 FAIL).
  A `data class`'s `equals`/`hashCode`/`toString`/`componentN` are Kotlin **language** semantics, so
  they are synthesized in **AST→IR lowering** (`Lower::synth_data_members`) as ordinary `IrFunction`s
  with IR bodies — *not* hand-written JVM bytecode — and registered in the class's method table so
  calls resolve and the generic method emitter handles them (a JS/other backend would get them for
  free). `equals` is `if (other !is T) return false; if (f != o.f) return false; … return true`
  (early-return chain — no value-position `&&` whose temp would leak into a merge frame); IEEE-aware
  via `Double/Float.compare`, structural ref-compare via the reference `Ne` path. `hashCode` is the
  `31*r + h(f)` fold (`{Double,Long,Float,Boolean}.hashCode`/`Objects.hashCode`); `toString` a
  `String.plus` chain. Fixed a latent bug: a `val b: A? = null` local was typed `Ty::Null` (so a
  reference `!=` took the `if_icmpne` primitive path) — locals now resolve a declared class type.
  `copy` (needs default args) is deferred, not faked.

- ✅ **Phase 156 — exhaustive `when` as a value + And/Or temp-leak fix** (140 → 146 box()=OK, 0 FAIL).
  A no-`else` `when` used as a value is only accepted by the checker when exhaustive (every enum entry
  / both booleans / sealed hierarchy), so the IR drops its **last arm to the `else`** — behavior-
  preserving, since one arm always matches. Fixed a real codegen bug this exposed: the value-position
  `&&`/`||` materialization parked its lhs in a temp slot that was inserted into the slot map
  **permanently**, leaking into later merge-point StackMapTable frames (a `false`/`else` path that
  never assigned the temp hit a frame claiming it defined → VerifyError). The temp is now removed
  after the `iand`/`ior` (dead; `next_slot` stays monotonic, no reuse). Guards (skip, never
  miscompile): a branchy `when` **subject** or arm **condition** (`when (when …)`, `x == when{…}`) —
  emitted while operands sit on the stack, their merge frames would omit them; a proper fix is a
  subject/condition temp.

- ✅ **Phase 157 — spill branchy operands to temps (root-cause fix)** (146 → 147 box()=OK, 0 FAIL).
  The recurring bug behind several `is_branchy` bail-guards: an expression that records a StackMapTable
  frame (a primitive comparison, `when`, `while`) can't be emitted while other operands sit on the
  stack — its merge frame omits them (VerifyError). Added `Emitter::records_frame(e)` (recurses the IR
  subtree for frame-recording nodes) and, in `New` and the enum `<clinit>` entry construction, when an
  argument records a frame, evaluate all args into temps **first** (clean stack) then construct. This
  retires the branchy-enum-entry-arg guard (`X(1 == 1)` now compiles). The same `records_frame` spill
  should next be applied to `MethodCall`/`Call` argument lists.

- ✅ **Phase 158 — finish the operand spill + single-eval branchy `when` subject** (147 → 148, 0 FAIL).
  Generalized the Phase-157 spill into `Emitter::emit_operands` and applied it to `MethodCall`
  (receiver+args) and local `Call` arg lists, completing the root-cause fix across every call site.
  In lowering, a *branchy* `when` subject (`when (when …)`) is now evaluated **once** into a temp
  (correct for side-effecting subjects too), retiring the branchy-subject bail-guard; a plain subject
  is still re-evaluated per comparison (which stays correct for a smart-cast local, whose slot type
  differs from its static type and would be mis-framed by a temp store).

- ✅ **Phase 159 — spill `emit_compare` operands; complete + correct the spill** (148 → 149, 0 FAIL).
  Applied the spill to `emit_compare` (both the `Objects.equals` and primitive paths), retiring the
  last branchy-operand guard — the branchy `when` **condition** (`x == when{…}`) now compiles. Fixed a
  latent correctness bug in the spill itself: an earlier operand's temp is **live** while a later
  branchy operand records frames, so the temps must be in `self.slots` during that window (else those
  frames mark the slot `Top` → "Bad local variable type"). Centralized into `spill_to_temps` (registers
  each temp in `self.slots`, caller removes after load); `New`/`MethodCall`/`Call`/enum-`<clinit>`/
  `emit_compare` all share it. The branchy-operand-on-non-empty-stack VerifyError class is now fully
  closed.

- ✅ **Phase 160 — class inheritance** (149 → 155 box()=OK, 0 FAIL). The biggest single lever
  (the `class-nonsimple` bucket). A `class B(…) : A(args)` where `A` is a simple/open class in the same
  file now lowers: `IrClass` gained `super_args`; `is_simple_class` allows a file base class; the ctor
  emits `super(args)` (spill-aware) against the base's parameter descriptor instead of
  `Object.<init>`; the class file's super_class is the base and an extended class is emitted non-`final`.
  Inherited member access walks the superclass chain (`resolve_field`/`resolve_method`, returning the
  *owning* class), and method calls keep `invokevirtual` so overrides dispatch dynamically. Guards
  (skip, never miscompile) for what still needs more: an override with a **different erased signature**
  (generic/covariant — needs a synthetic JVM **bridge**), and a **property override** (`override val`
  — needs getter/setter dispatch, which krusty's direct-field model lacks). Base from a classpath/Java
  type, secondary constructors, and `abstract` classes also stay out for now.

- ✅ **Phase 161 — bridge-method synthesis** (155 → 164 box()=OK, 0 FAIL). An override whose erased
  signature differs from the supertype's (a generic or covariant override) now gets a synthetic
  `ACC_BRIDGE|ACC_SYNTHETIC` method (in `IrClass.bridges`, recorded in lowering instead of bailing).
  `emit_bridges` emits each with the supertype's erased descriptor: it adapts every argument
  (checkcast a reference, unbox a primitive, numeric-convert) and the return value (box / convert),
  delegating via `invokevirtual` to the concrete override. Straight-line code (no frames). Unblocks
  the `bridges/*` generic/covariant-override tests.

- ✅ **Phase 162 — interfaces (+ interface bridges)** (164 → 191 box()=OK, 0 FAIL). The biggest single
  jump. An `interface` with abstract methods emits as `ACC_PUBLIC|INTERFACE|ABSTRACT` with one
  `public abstract` method each (no ctor/fields). A class `: I` lists `I` in its `implements`;
  `IrClass.interfaces` carries them. Method calls through an interface-typed receiver use
  `invokeinterface`. Interface bridges: for each implemented-interface method whose erased signature
  differs from the class's actual implementation (declared **or inherited** — `resolve_method` walks
  the superclass chain, so fake-override/diamond cases work), a bridge with the interface's descriptor
  delegates to the impl (deduped against the base-class bridges). Still out: interface **default
  methods** (need a `DefaultImpls` class) and interface **properties** (abstract getters).

- ✅ **Phase 163 — abstract classes + unqualified `this.method()`** (191 → 214 box()=OK, 0 FAIL). An
  `abstract class` is now accepted: its abstract methods (no body) are declared `ACC_ABSTRACT` (the
  class gets `ACC_ABSTRACT`, non-`final`), concrete methods emit normally, and subclasses extend it via
  the existing inheritance path. Also added unqualified instance-method calls inside a class body
  (`foo()` → `this.foo()`, resolving through the superclass chain) — a common gap that this unblocked
  broadly. Fixed a data-class corner: a data class no longer synthesizes `equals`/`hashCode`/`toString`
  when a superclass already declares it (e.g. a base's `final override fun toString()`), inheriting it
  instead of regenerating.

- ✅ **Phase 164 — objects (named singletons)** (214 → 217 box()=OK, 0 FAIL). `object Foo { … }` now
  emits as a class with a `public static final Foo INSTANCE` field, a private no-arg constructor (body
  properties initialized in it), and a `<clinit>` that builds the instance. A bare `Foo` reference
  lowers to `IrExpr::ObjectInstance` (`getstatic INSTANCE`); `Foo.x`/`Foo.f()` read/call through it
  (the checker types a bare object name as `Error`, so `recv_ty` maps an object-name receiver to its
  object type). Guard: an object with an `init { … }` block is skipped (a `const val` read must not
  trigger the init — krusty doesn't model const-inlining).

- ✅ **Phase 165 — default arguments (positional, constant-literal)** (217 → 218 box()=OK, 0 FAIL). A
  top-level function call that omits trailing arguments now fills them from **constant-literal**
  defaults at the call site (`fun f(x: Int = 5)` called `f()` → `f(5)`). Guards (skip, never
  miscompile): a non-literal default (referencing other params / `this` — needs the `$default`
  synthetic method) and a call mixing **named arguments** with omitted defaults (the IR sees args in
  source order, not the checker's reordered positions). The full `$default` mechanism (mask + synthetic
  method) and named-argument reordering are the follow-ups that would generalize this.

- ✅ **Phase 166 — named arguments + defaults (functions & constructors)** (218 → 226 box()=OK, 0 FAIL).
  `lower_args_defaulted` now places each argument into its parameter slot — a positional arg fills the
  next free position, a named arg (`x = …`) fills its named parameter (resolved against the callee's
  parameter names) — then fills unprovided slots from constant-literal defaults. Applied to top-level
  function calls and constructor calls (so `C(y = 1, x = 2)`, `foo(b = 2)`, annotation-style named ctor
  args, and `C()`/`f()` with defaults all work). Arguments are still evaluated in slot order (fine for
  the side-effect-free common case). Non-literal defaults (need `$default`) and instance-method default
  args remain follow-ups.

- ✅ **Phase 167 — safe calls `a?.b` / `a?.m(...)`** (226 box()=OK, 0 FAIL — corpus-neutral, real feature).
  Lowered in the front-end (backend-agnostic) to `{ val t = recv; if (t != null) t.member else null }`:
  a temp holds the receiver, a `null` guard selects the member access (`GetField` / `MethodCall`)
  against the non-null receiver, else `null`. Composes with Elvis (`a?.m() ?: d`) and chains through the
  existing `when` lowering. Required fixing `value_ty_of_when`: a `null`/`Nothing` last branch (the
  no-receiver arm) carries no concrete type and verify-typed the merge stack as `top`, tripping
  `VerifyError: Bad type on operand stack`; it now uses a concrete branch type (a reference) for the
  merge frame, since `null` is assignable to any reference. Covered by `tests/safe_call_e2e.rs`
  (round-trip vs the JVM under `-Xverify:all`). Resolves to user-defined methods/properties; **stdlib**
  receivers (`s?.substring(1)`) still bail — they need the external-call path and are a follow-up.

- ✅ **Phase 168 — invokedynamic + BootstrapMethods class-writer infrastructure** (226, 0 FAIL).
  Added the constant-pool kinds `MethodHandle`/`MethodType`/`InvokeDynamic`, a `BootstrapMethods`
  class attribute, and the `invokedynamic` opcode + emitter API (`method_type`,
  `method_handle_static`, `add_bootstrap`, `invoke_dynamic`). Purely additive — the foundation for
  indy lambda/callable-ref lowering. Validated by `tests/indy_infra_e2e.rs` (a hand-built
  `LambdaMetafactory` lambda over `java.util.function.IntUnaryOperator`, run under `-Xverify:all`).

- ✅ **Phase 169 — non-capturing lambdas** (226 → 234 box()=OK, 0 FAIL). A lambda literal
  `{ a -> … }` lowers to `IrExpr::Lambda` → `invokedynamic` + `LambdaMetafactory.metafactory`
  producing a `kotlin/jvm/functions/Function{arity}`; the body becomes a synthesized `private static`
  facade method `<enclosing>$lambda$<n>` with the lambda's real parameter types (the checker already
  infers these via `lambda_param_types`). Calling a function value `f(args)` lowers to
  `IrExpr::InvokeFunction` → `FunctionN.invoke` (args boxed to `Object`, the `Object` result
  cast/unboxed to the return type). `Ty::Fun` now maps to `FunctionN`. The impl method uses primitive
  specialization with a boxed `instantiatedMethodType`, so `LambdaMetafactory` inserts the box/unbox
  adapter (matching kotlinc). Guards (skip, never miscompile): capturing lambdas (body reads an
  enclosing local), lambdas inside class methods (could capture `this`/fields), `Unit`/`Nothing`
  returns (need the `kotlin/Unit` singleton), and lambda arguments to a **generic** function
  (type-parameter erasure needs a call-site checkcast not yet modeled). `tests/lambda_e2e.rs`.
  Follow-ups: capturing lambdas (indy call-site args), `Unit` lambdas, generic/suspend consumers,
  callable references (same indy infra).

- ✅ **Phase 170 — unbound top-level function references `::foo`** (234 → 235 box()=OK, 0 FAIL).
  `::foo` reuses the lambda machinery: it lowers to `IrExpr::Lambda` whose `impl_fn` points directly at
  the referenced function (no synthesized body), so `invokedynamic` + `LambdaMetafactory` bind the
  function handle as a `FunctionN`. (kotlinc emits a `FunctionReferenceImpl` subclass with reflection
  metadata, but that class is synthetic/non-ABI — the facade's public signatures and the round-trip
  result are identical.) Same guards as lambdas (`Unit`/`Nothing` return, generic referenced function),
  plus an **arity > 22** limit. Bound/object/constructor references still bail. `tests/callable_ref_e2e.rs`.
  Architecture: a function type lowers to the **structural** `IrType::Function { params, ret }` (no JVM
  package name in common lowering); the JVM backend maps it to `kotlin/jvm/functions/FunctionN` and owns
  the fixed-arity `Function0..22` constraint — a JVM detail, not a language one. That constraint is
  enforced inside `emit_all` (now returning `Option`, `None` when unrepresentable), so no emission path
  (backend or conformance harness) can bypass it.

- ✅ **Phase 171 — not-null assertion `x!!`** (235 → 236 box()=OK, 0 FAIL). `IrExpr::NotNullAssert`
  emits `dup` + `kotlin/jvm/internal/Intrinsics.checkNotNull(Object)V` (the value stays on the stack,
  the duplicate is consumed by the null check) — matching kotlinc. On a non-null primitive operand `!!`
  is a no-op. `tests/not_null_assert_e2e.rs`.

- ✅ **Phase 172 — classpath-class construction + `throw`** (236 → 245 box()=OK, 0 FAIL). `IrExpr::
  NewExternal { internal, ctor_desc, args }` constructs a non-IR class (`new` + `dup` + args + `invoke
  special <init>`); the constructor descriptor comes from the classpath (`resolve_java_ctor`), with a
  fallback for `Throwable` types (every JDK throwable has `()`/`(String)` constructors) since the
  classpath reader doesn't read jimage constructor descriptors yet. `IrExpr::Throw` emits `athrow` and
  counts as diverging. Together these unblock `throw RuntimeException("…")` and exception/value
  construction broadly (+9 — the largest single-phase jump since interfaces). Constructors whose
  descriptors live only in the JDK jimage (e.g. `StringBuilder()`) still bail. `tests/throw_e2e.rs`.

- ✅ **Phase 173 — try/catch + `throw`-exposed fixes** (245 → 256 box()=OK, 0 FAIL). `IrExpr::Try`
  (no `finally`) stores the body value (and each catch value) into a result temp and loads it at the
  merge — mirroring kotlinc; each catch is an exception-table handler with a frame carrying the caught
  exception on the stack and the pre-`try` locals. Enabling it surfaced four latent bugs, all fixed:
  (a) `String.plus` didn't spill a branchy operand (`"O" + try`), so the `StringBuilder` was live across
  its merge frames; (b) a diverging body/catch (`throw`) still emitted a dead value `store`;
  (c) a class with a diverging `init { throw … }` emitted a dead trailing `return` in `<init>`;
  (d) `as T` to a non-null reference type didn't null-check, so it passed `null` — now emits
  `Intrinsics.checkNotNull(value, "null cannot be cast to non-null type …")` then `checkcast`
  (`IrTypeOp::CastNonNull`, matching kotlinc). Also added constant-folding of a literal-boolean `if`
  condition (`if (false) { … }`) — emit only the taken branch, like kotlinc's dead-code elimination.
  try in a property initializer is skipped (ctor frame context). `tests/try_catch_e2e.rs`.

- ✅ **Phase 174 — generic-erasure call-site checkcast** (256 → 261 box()=OK, 0 FAIL). A generic
  function (`fun <T> id(x: T): T`) erases its type-parameter return to `Object` in the JVM signature;
  the call site must `checkcast` the result to the inferred concrete type (kotlinc does — krusty
  previously returned the `Object` directly, a latent `VerifyError: Bad return type` miscompile).
  `lower_arg` now inserts a `checkcast` when an erased-`Object` value flows into a more specific
  reference target; val initializers, `return` statements (via a new `Lower.cur_ret_ty`), and the
  expression-body return all route through it. This let the Phase 169 lambda-to-generic guard be
  removed (`privateConst`, `syntheticAccessor`, …). Also fixed `IrExpr::InvokeFunction` to spill a
  branchy argument to temps (a function value was live across the arg's merge frames —
  `operation(if (…) a else b)`). `tests/generic_fn_e2e.rs`.

- ✅ **Phase 175 — `try … finally`** (261 → 263 box()=OK, 0 FAIL). `IrExpr::Try` gains a `finally`
  block, inlined (as kotlinc does) at each exit: the normal fall-through, the end of each catch, and a
  synthetic catch-all (`catch_type` 0) covering the body + all catch handlers that runs the `finally`
  then re-throws. A diverging `finally` (`finally { throw }`) suppresses the dead `goto`s. Bails when a
  `return`/`break`/`continue` would exit the `try` before the `finally` runs (`body_has_nonlocal_exit`,
  loop-depth-aware so a loop-local `break` is fine), and a nested `try` inside the `finally` is rejected
  by the checker (it would be emitted multiple times). `tests/finally_e2e.rs`.

- ✅ **Phase 176 — `vararg` + array `for`-iteration** (263 → 264 box()=OK, 0 FAIL). A `vararg`
  parameter's JVM type is the array; the call site packs the trailing arguments into a fresh array via
  the new `IrExpr::Vararg { element_type, elements }` (Kotlin IR's `IrVararg`; the JVM backend emits
  `newarray`/`anewarray` + per-element `dup`/index/store) and passes it (matching kotlinc). Spread (`*arr`) and a branchy element are skipped. `for (x in arr)`
  over an array now lowers to an index loop (`i = 0; while (i < arr.size) { x = arr[i]; …; i++ }`, with
  the array/size hoisted) — the complement that consumes a vararg array. `tests/vararg_e2e.rs`.

- ✅ **Phase 177 — companion object methods** (264 → 268 box()=OK, 0 FAIL). A `class C` with a
  `companion object { fun … }` now compiles (like kotlinc) to a synthesized `C$Companion` class holding
  the companion methods as instance methods, a `public static final Companion` field of that type on
  `C` built in `C`'s `<clinit>`, and `C.foo(args)` → `getstatic C.Companion; invokevirtual`
  (`IrExpr::CompanionInstance`). The companion's constructor is package-private (so `C`'s `<clinit>` can
  call it without nestmate attributes — kotlinc uses `private` + a `DefaultConstructorMarker` ctor; a
  byte-parity gap). Companion **properties** (`val`/`const val`, whose backing fields live on the outer
  class) are not yet modeled — such a class is skipped. `tests/companion_e2e.rs`. Also: constructor
  `Intrinsics.checkNotNullParameter` (non-null reference primary-ctor params, emitted before `super()`)
  — a simple class's `<init>` is now byte-identical to kotlinc.

- ✅ **Phase 178 — computed properties (custom getters)** (268 → 270 box()=OK, 0 FAIL). A property with
  a custom getter and no backing field (`val x: T get() = expr`) compiles to a `getX()` accessor; reads
  call it. Top-level → static `getX()` on the facade (read → `invokestatic`); class body property →
  instance `getX()` (`obj.x` → `invokevirtual getX()`, unqualified `x` inside the class → `this.getX()`).
  Accessor name is `getX` (an `is`-prefixed boolean keeps its name). Computed body properties are
  excluded from the class fields, and the constructor init-order skips them. `tests/computed_prop_e2e.rs`.
  Also unified `ObjectInstance`/`CompanionInstance` into one `IrExpr::StaticInstance { owner, ty, field }`
  (Kotlin IR's `IrGetObjectValue` — both are a `getstatic` of a singleton static field).

- ✅ **Phase 179 — default property accessors (private field + `getX()`/`setX()`)** (270, 0 FAIL —
  byte-parity). Every backing-field property of a normal class now gets a synthesized public `getX()`
  (and `setX()` for `var`) accessor whose body reads/writes the (now **private**) field, and property
  access from **outside** the declaring class is routed through the accessor (`recv.x` →
  `invokevirtual getX()`, `recv.x = v` → `setX(v)`, including safe calls `r?.x`); inside the class the
  field is read/written directly. A simple class's field + accessors + external access now match
  kotlinc (remaining gaps: `final` on a `val` field/accessor; object/enum properties still use public
  fields + direct access — accessors for them are a follow-up).

- ✅ **Phase 180 — default arguments via the `$default` mechanism** (272 → 275 box()=OK, 0 FAIL,
  byte-parity). A parameter's default *value* is backend-agnostic IR (`IrFile.fn_param_defaults: FunId →
  Vec<Option<ExprId>>`). The JVM backend realizes it by emitting a `name$default(self, params…, int
  mask, Object marker)` synthetic stub (`if ((mask & (1<<i)) != 0) param = <default>;` then tail-call the
  real method — using the bitwise ops added in the previous phase). Data-class `copy(y = 5)` was the
  first user: each `copy` parameter defaults to the receiver's property, so `copy` + `copy$default(P,
  …, int, Object)` are byte-identical to kotlinc. The checker maps named/omitted arguments onto
  parameters (`map_call_args`) for any method whose signature has defaults (`required < params`) — not a
  `copy` special-case. `tests/data_copy_e2e.rs`.

- ✅ **Phase 181 — defaulted call = a call with holes; instance methods** (275 → 277 box()=OK, 0 FAIL,
  byte-parity). A call that omits arguments is *not a new operation* — it is an ordinary call where some
  arguments are absent (Kotlin's own IR lets an `IrCall` argument be null). So the separate
  `IrExpr::DefaultedCall` is removed and folded into `MethodCall { …, args: Vec<Option<ExprId>> }`:
  `args[i] = None` means parameter `i` is omitted and takes its default; all-`Some` is an ordinary full
  call. The JVM backend emits the `$default`-stub invocation when any argument is `None`, an ordinary
  `invokevirtual`/`invokeinterface` otherwise; JS passes `undefined` for a hole (native defaults). This
  generalizes defaults from `copy` to any instance method (`fun add(a: Int, b: Int = 10)`); param→arg
  mapping uses `IrFile.fn_param_names` (recorded for defaulted functions). Out of model (so the file
  skips, never miscompiles): interface defaults (kotlinc routes those through `$DefaultImpls`) and >31
  parameters (kotlinc's multi-`int` mask). `tests/default_args_member_e2e.rs`. Architecture: default
  *meaning* in IR (a call with holes), `$default` *stub* + mask in the JVM backend.

- ✅ **Phase 182 — `in` / `!in` range membership** (277 → 278 box()=OK, 0 FAIL). The membership
  operator was unparsed (`x in 1..10` → "expected ')'", blocking ~22 `ranges/` files at the parse stage).
  Added it at comparison precedence (bp 7, beside `is`/`!is`). A range RHS (`a..b`, `a until b`,
  `a downTo b`) parses to `Expr::InRange { value, start, end, kind, negated }`; a non-range RHS becomes
  `container.contains(value)` (`!in` wraps in `!`). Lowering desugars `InRange` to temps — the bounds
  then the value are each evaluated once, in source order (matching kotlinc's `start..end` then
  `.contains`) — followed by a comparison chain (`lo <= v && v <(=) hi`); `!in` uses the De Morgan dual
  so no logical-not node is needed. `downTo` swaps the bounds (membership is `end <= v <= start`). The
  checker requires uniform primitive operand types (mixed Int/Long ranges would need promotion not yet
  modeled) and types it `Boolean`. Net +1 (the `ranges/` corpus needs more — `IntRange` objects,
  unsigned types, collections), but `in` is pervasive and foundational.

- ✅ **Phase 183 — `break` / `continue`** (278 → 285 box()=OK, 0 FAIL). Loop control was unmodeled —
  any loop using it bailed. Added `IrExpr::Break`/`Continue` and a `loop_stack` of
  `(continue_label, break_label)` in the JVM backend; `break` → `goto end`, `continue` → `goto cont`.
  `IrExpr::While` gained an `update: Option<ExprId>` (a `for`-loop's increment) emitted at the `continue`
  label, so `continue` advances the counter instead of skipping it; a plain `while` has `update: None`
  (then `continue` re-tests the condition). Also fixed a pre-existing limitation: loop bodies ending in
  an expression (`…; if (c) break`) parse it as the block's `trailing` expr — the three loop lowerings
  now keep it as a discarded statement instead of bailing. `break`/`continue` in *value* position
  (`s += if (c) x else break`) needs operand-spilling the emitter doesn't do, and across a `try`/lambda
  needs region-crossing — those are gated by `bc_complex_e` (a context-propagating AST walk) so the file
  skips rather than miscompiling. `tests/break_continue_e2e.rs`. (Follow-ups: `++`/`--` are parsed
  (`Stmt::IncDec`) but not yet lowered; labeled break/continue; value-position via operand spill.)

- ✅ **Phase 184 — `++` / `--` (statement position)** (285 → 291 box()=OK, 0 FAIL). `Stmt::IncDec` was
  parsed but never lowered (any `i++` bailed). Lowered `name++`/`name--` on a local numeric/`Char`
  variable to `name = name ± 1` (in statement position the pre/post distinction is unobservable). The
  checker now also accepts `Char` (`c++` → `c.inc()`). A `var` field/property target or a user
  `operator inc`/`dec` still bails (skipped, not miscompiled). Unblocks the common `while (…) { i++ }`
  counter idiom. (Follow-up: `++`/`--` in expression position, and on fields/properties.)

- ✅ **Phase 185 — `do … while`** (291 → 296 box()=OK, 0 FAIL). Added the `KwDo` keyword,
  `Stmt::DoWhile`, and a `post_test: bool` on `IrExpr::While` (one loop node parameterized by where the
  condition is tested) — the JVM emit skips the top test and tests at the bottom (`ifne start`), so the
  body always runs once; `continue`/`break` reuse the Phase-183 `loop_stack`. JS emits a native
  `do { } while`. Enabling it surfaced a **pre-existing smart-cast bug** (independent of loops):
  `if (o is String) return o.length` emitted the receiver as its wide slot type (`Any`) without a
  `checkcast` to `String` → `VerifyError`. The `String.length` intrinsic now checkcasts a smart-cast
  receiver, like the user-field path already did. `tests/do_while_e2e.rs`. (The same smart-cast checkcast
  is still missing on other stdlib-intrinsic receivers — not yet hit by a compiling box file.)

- ✅ **Phase 186 — primitive conversions + `\uXXXX` escapes** (296 → 313 box()=OK, 0 FAIL). Primitive
  numeric/`Char` conversion calls (`n.toLong()`, `c.toInt()`, `i.toChar()`, `n.toByte()`, …) were typed
  by the checker but never lowered — they all bailed. Lowered them to `ImplicitCoercion` (the backend
  already emits `i2l`/`l2i`/`i2c`/… via `emit_num_conv`); the checker now also allows them on `Char`, and
  `c.code` (a property → `Int`). This unblocked +17 files. Enabling it surfaced a real **lexer bug**:
  `\uXXXX` unicode escapes weren't processed (`unescape_chunk`/`unquote_char` fell through to a literal
  `u`), so a string like `"0…"` was 3× too long and string comparisons failed. Added `\uXXXX`
  (plus `\b`, `\'`, `\0`) to both string and char unescaping. (Also confirmed the conformance gate links
  the **2.4.0 dist stdlib** via `dist_jar`, not the gradle 2.0.21 jar — only my ad-hoc smoke commands
  had used 2.0.21.)

- ✅ **Phase 187 — top-level extension functions** (313 → 315 box()=OK, 0 FAIL). The checker already
  resolved extension calls and bound `this`; only the backend was missing. `fun Recv.name(…)` now lowers
  to a static method whose first parameter is the receiver (Kotlin's strategy), keyed by
  `(receiver descriptor, name)` in a new `Lower.ext_fun_ids` (separate from `fun_ids` since `Int.foo` and
  `String.foo` share a name). A call `recv.name(args)` → a static call with the receiver prepended; the
  body binds `this` to parameter 0. Fixes to support it: the overload-clash check now includes the
  extension receiver in the JVM signature key (so `Int.foo`/`String.foo` don't collide) and exempts
  extensions from the by-name "can't dispatch overloads" gate (they dispatch by receiver). A user
  `operator fun T.plus(…)` (etc.) extension now overrides the builtin operator in the `Binary` lowering
  (fixes `kt889`). A receiver that doesn't resolve to a concrete type (a generic `T.foo()`) bails rather
  than guessing `Object`. `tests/extension_fun_e2e.rs`. This is the foundation for resolving stdlib
  extension functions (`kotlin.ranges.until`/`downTo`/`step`) by symbol — the proper, non-hardcoded path
  to range support.

- ✅ **Phase 188 — stdlib multifile-facade resolution** (315 box()=OK, 0 FAIL; foundational, +0 box).
  The stdlib's extension/top-level functions don't live on the public facade class — the facade
  (`kotlin/text/StringsKt`, `kotlin/ranges/RangesKt`) is **empty and extends a chain of package-private
  multifile *part* classes** (`StringsKt___StringsKt` → `StringsKt__StringsKt` → …) that hold the actual
  `public static` methods. krusty's classpath extension index scanned each class's own public methods and
  skipped non-public classes, so it found *nothing* in the stdlib — every stdlib extension was
  "unresolved". Rewrote `ensure_ext_index` as two passes: collect every class (public or not), then for
  each **public** class index the static methods reachable through its **superclass chain** (the parts),
  with `owner` = the public facade — which is what kotlinc emits (`invokestatic StringsKt.repeat`,
  verified). `1.until(10)` now resolves (was "unresolved method"). Remaining for actually compiling these
  calls: match the receiver against its **supertype chain** (kotlinc's `repeat` is a `CharSequence`
  extension, called on a `String`) and a lowering path that emits `invokestatic facade.name(recv, …)`.

- ✅ **Phase 189 — resolved stdlib extension calls** (315 → 317 box()=OK, 0 FAIL). Added
  `Callee::Static { owner, name, descriptor }` — a general `invokestatic owner.name:descriptor` carrying
  the **resolved** JVM descriptor, so no stdlib name is hardcoded in the backend. The member-call
  lowering now falls back to `resolve_extension` (the Phase-188 classpath index): a `recv.name(args)`
  that resolves to a classpath extension becomes `invokestatic facade.name(recv, args…)` — owner and
  descriptor from the classpath, like kotlinc. `5.coerceAtLeast(3)`, `5.coerceIn(1,3)` (real
  `kotlin.ranges` extensions) now compile, resolved not hardcoded. The ext-index was also made lean
  (retain only `(super_class, public-static method sigs)` per class, not full `ClassInfo`). Still needed
  for `String`/collection extensions: receiver-supertype matching (`String.repeat` is a `CharSequence`
  extension), and the range loop-optimization keyed on the resolved `kotlin.ranges` symbol.

- ✅ **Phase 190 — read interfaces + receiver-supertype extension matching** (317 box()=OK, 0 FAIL;
  foundational, +0). The classreader now captures a class's `interfaces` (it discarded them).
  `resolve_extension` walks the receiver type's **supertype chain** (superclass + interfaces, BFS,
  most-specific first) so an extension declared on a supertype resolves — kotlinc's `String.repeat` is a
  `CharSequence` extension (`StringsKt.repeat(Ljava/lang/CharSequence;I)`). Works for receivers krusty
  can read (Kotlin stdlib types / user classes in jars). **Blocked for JDK receivers** (`String` →
  `CharSequence`): `Classpath::find` returns `None` for `Entry::Jimage` — krusty doesn't yet read class
  bytes from the JDK jimage (`lib/modules`), so `String`'s interfaces are unknown. Reading JDK class
  bytes (jimage, or the simpler `jmods/*.jmod` zips) is the next prerequisite for `String`/`CharSequence`
  extension calls.

- ✅ **Phase 191 — classpath instance-method resolution + lowering** (317 box()=OK, 0 FAIL;
  foundational). `resolve_java_instance` now walks the receiver type's **super/interface chain** (an
  instance method may be inherited — `IntRange.iterator()` is on `IntProgression`/`Iterable`). Added
  `Callee::Virtual { owner, name, descriptor, interface }` and a member-call lowering fallback: a call on
  a classpath-class receiver resolves to a real instance method and emits `invokevirtual`/
  `invokeinterface recvType.name:descriptor` (descriptor from the classpath — no hardcoded names). This
  is the mechanism the **for-loop iterator protocol** needs (`e.iterator()`/`hasNext()`/`next()`).
  +0 box for now because most instance-method receivers are **JDK types** (`String`, `StringBuilder`,
  `List`) whose bytes krusty can't read — `Classpath::find` returns `None` for the jimage. **Reading JDK
  class bytes (jimage `lib/modules`, or the `jmods/*.jmod` zips) is the one prerequisite now blocking:
  String/CharSequence supertype matching, JDK instance calls, and the general iterator-protocol for-loop
  that replaces the parser-hardcoded range path.**

- ✅ **Phase 192 — read JDK class bytes from the jimage** (317 → 321 box()=OK, 0 FAIL). The big
  unblocker: `Classpath::find` returned `None` for the JDK jimage, so `String`/`StringBuilder`/`List`
  (and `String`'s `CharSequence` interface) were unreadable — blocking supertype matching and JDK
  instance calls. The jimage (`lib/modules`) stores classes **uncompressed**, so a one-time
  name→`(offset,size)` index + a seek-read extracts them (`build_jimage_index`, mirroring the existing
  `scan_types_jimage` navigation). `"hi".repeat(3)` (resolves `String`→`CharSequence`→`StringsKt.repeat`)
  and `StringBuilder().append(…)` instance calls now compile — **by resolution from the classpath, no
  hardcoded names**. The index is cached process-globally (`global_jimage_cache`) so the 146 MB parse
  happens once (gate 10.5s→14.5s, still <60s). Enabling JDK resolution surfaced a pre-existing miscompile
  (`kt1721`: invoking a function-typed *field* `f()` emitted a bogus `new Object()`) — gated (bail) until
  function-value fields are modeled. `tests/java_instance_e2e.rs` now puts the stdlib on its run-cp
  (emitted code references `Intrinsics`, like kotlinc). This is the foundation for the iterator-protocol
  for-loop (`IntRange.iterator()`/`hasNext()`/`next()` now readable).

- ✅ **Phase 193 — interface delegation (`: I by d`)** (321 → 325 box()=OK, 0 FAIL). Delegation is
  sugar: the class forwards each of `I`'s methods to the delegate. The parser captures `(iface, delegate)`
  for a simple `val`-parameter delegate (`ClassDecl.delegations`); the backend synthesizes a forwarder
  `fun m(args) = this.delegate.m(args)` (an `invokeinterface` on the delegate field) per interface
  method, via `synth_delegation_forwarders` (reusing `add_synth_method`). `lookup_method` now walks
  implemented interfaces so the delegating class's calls type-check. Non-`val`/classpath-interface
  delegation bails (skips). `tests/` covered by the conformance gate.

- ✅ **Phase 194 — read the generic `Signature` attribute (generics foundation)** (325 box()=OK, 0 FAIL;
  foundational, +0). kotlinc's JVM generics are **erasure**: each type parameter erases to its
  upper bound (default `Object`), and the generic info is written to the bytecode `Signature` attribute.
  krusty already erases (generic classes/functions compile); the missing half is the generic type
  *arguments*. Step 1: the classreader now captures the class-level `Signature` attribute
  (`ClassInfo.signature`) — e.g. `IntRange` →
  `Lkotlin/ranges/IntProgression;Lkotlin/ranges/ClosedRange<Ljava/lang/Integer;>;…`, so a generic
  supertype's type argument (`ClosedRange<Int>` → element `Int`) is recoverable. The metadata reader was
  refactored to accumulate both `@Metadata.d2` and `Signature` without early-returning (no regression to
  type-alias resolution). Next on the generics arc: a signature-parse helper → generic supertype/element
  types → the iterator-protocol for-loop → de-hardcoded ranges/collections.
- **Phase 195** made `Ty::Obj` carry a (interned) generic argument slice (`Ty::obj_args`,
  `Ty::type_args()`) — the architectural core, behaviour-neutral (all sites passed empty args).
- **Phase 196** populates those arguments from *declared* types: the parser now captures the full
  `<…>` list on a class type into `TypeRef.targs` (instead of discarding it), and the checker's
  `resolve_ty`/`ty_of_ref` build `Ty::obj_args(internal, [resolved args])` for a generic instantiation
  (`val m: Map<String, Int>` → `Obj("…/Map", [String, Int])`). Still JVM-erased in descriptors, so
  behaviour-neutral (325/0-FAIL); the arguments are now *present* on declared-typed values. Next:
  consume them — substitute a class's type parameters at member access (`Box<Int>().x : Int`), with the
  emit side inserting the generic-read checkcast/unbox kotlinc emits.
- **Phase 197** consumes the arguments: a property declared as a bare type parameter is substituted at
  member access (`ClassSig.generic_props`, `check_member`), and `coerce_generic_read` inserts the
  checkcast/unbox kotlinc emits on the erased read. e2e covers primitive/reference/multi-param cases.
- **Phases 198–202 — front-end/back-end decouple.** The compiler core must speak Kotlin types and
  depend on no JVM backend (multiplatform: JVM bytecode now, Kotlin/JS via klib later).
  - 198: the erased top type is `kotlin/Any` in the core, mapped to `java/lang/Object` only at JVM
    emit chokepoints (`jvm_class_map::to_jvm_internal`/`to_kotlin_internal`). `Any`/`String` are
    distinct Kotlin builtins, not typealiases for the Java types.
  - 199: the String/StringBuilder resolvers drop their (unused) JVM descriptors and return only `Ty`.
  - 200: a primitive array element boxes via the backend wrapper map, not an inline literal.
  - 201: a **`LibrarySet`** trait (`src/libraries.rs`) is the common denominator a front end needs
    from a target's compiled libraries — one half of a *platform* (the emitter is the other). The
    JVM impl (`jvm::jvm_libraries::JvmLibraries`) owns all classpath reads / descriptor parsing /
    name normalization. `SymbolTable` holds a `Box<dyn LibrarySet>`; resolve/ir_lower resolve through it.
  - 202: resolve.rs and ir_lower.rs hold **zero `crate::jvm` references**. Remaining java/lang in the
    core: `StringBuilder`, `Class`, the String supertype set; plus the `Ty::Array` boxing-model fix
    (keep `Array<Int>` element `Int`, box in the emitter) so the resolver stops computing wrappers.

- ✅ **Phase 265 — range expressions as values (`a..b`, `a..<b`)** (429 → 441 box()=OK, 0 FAIL).
  `..`/`..<` are the only range *operators* (parsed tighter than infix functions, looser than additive);
  `until`/`downTo`/`step` are de-special-cased back to ordinary stdlib infix functions. A new
  `Expr::RangeTo` types to `IntRange`/`LongRange`/`CharRange` and lowers to `new IntRange/LongRange(II/JJ)`
  (`..`) or `RangesKt.until` (`..<`); `.first`/`.last` resolve to the classpath getters. `for (x in r)`
  over a stored `Int`/`Long` range value iterates as a counted `getFirst()/getLast()` loop (no boxing);
  the loop variable's element type comes from `range_primitive_elem`. Also fixed a latent miscompile this
  unlocked: `listOf<Short>(1, 2)` would box `Int` literals as `Integer` and `ClassCastException` on a
  narrowing read — now cleanly skipped (the erased logical-vs-physical element type isn't tracked yet).
  `tests/range_value_e2e.rs`; SPEC §7.
- ✅ **Phase 266 — function types as generic arguments** (442 box()=OK). `ArrayList<() -> Unit>()`: the
  call-site generic-argument detector accepts the `(`/`)`/`->` of a function-type argument.
- ✅ **Phase 267 — `++`/`--` as expression values** (441 → 447 box()=OK). `Expr::IncDec` value node (no
  temp slot: old = new ∓ 1); also fixed an empty-`when` subject side-effect bug. `tests/incdec_expr_e2e.rs`.
- ✅ **Phase 268 — property type inference from a primitive conversion call** (447 → 448). `val b =
  2.toByte()` infers `Byte`; `x.toString()` infers `String`.
- ✅ **Phases 269–272, 275–276 — unsigned types `UInt`/`ULong`** (448 → 453 box()=OK). Literals, arithmetic,
  `Integer.{divide,remainder,compare}Unsigned`, `toUnsignedString`, boxing (`box-impl`/`unbox-impl`/
  `is UInt`), and `for`-ranges. The syntactic `for`-loop is generalized to `Int`/`Long`/`UInt`/`ULong`/`Char`
  counters. `tests/unsigned_e2e.rs`. (Reverted within 269: a hardcoded `Int.MAX_VALUE` table — kotlinc reads
  it from the stdlib `const val`, so it must come from the classpath, not krusty source.)
- ✅ **Phase 273 — reject mutable capture in extension-call lambdas** (a silent miscompile fix).
  `listOf(…).forEach { s += it }` was typed by a path that skipped the capture guard, lowering to a closure
  whose mutation was lost; now it bails (skip), never miscompiles.
- ✅ **Phase 274 — unbox primitive lambda parameters from the `FunctionN` signature**. `mapIndexed`'s index
  is `Int`, not boxed `Integer`. `tests/mapindexed_e2e.rs`.

- 🚧 **Phase 388 — value/inline classes, step 4: member synthesis** (886, codegen). The JVM emitter now
  emits `static` class members (`emit_class` passes `instance = !f.is_static`; `emit_method` already
  supported the no-`this` path used by top-level functions). A `@JvmInline value class X(val v: U)` is
  admitted to the IR path and synthesizes kotlinc's unboxed-support members on `X.class`:
  `box-impl(U):X` and `constructor-impl(U):U` (static, via the new `add_synth_static_method`) and
  `unbox-impl():U` (instance); the `U` field, `<init>(U)`, and `getV()` getter come from the ordinary
  single-field class path. The static `-impl` members carry `dispatch_receiver = Some(owner)` so they
  stay off the top-level facade. Verified against kotlinc 2.4.0 (`tests/value_class_e2e.rs`): the
  emitted descriptors + `ACC_STATIC` flags match (`box-impl(int):S` static-final, `constructor-impl(int):int`,
  `unbox-impl():int`, `getX():int`). Use-site unboxing isn't wired yet, so the resolver still rejects
  value-class *files* (they skip, not FAIL) — admission here is for synthesis; 886/0-FAIL.
  NEXT: (step 4b) the remaining members — `equals`/`hashCode`/`toString` + their `-impl`/`-impl0` forms,
  and the private `<init>` + `DefaultConstructorMarker` synthetic ctor — to fully match kotlinc's
  `X.class`; then (step 5) use-site unboxing lifts the rejection.

- 🚧 **Phase 387 — value/inline classes, step 3: symbol-table representation** (886, foundation).
  `ClassSig` gains `value_field: Option<(String, Ty)>` — for a `@JvmInline value class X(val v: U)`, the
  sole underlying property `(name, U)`, populated in `collect_signatures`. This is the data layer for the
  unboxed model: an `X` value is represented as its underlying `U`; `X.class` carries the static
  `box-impl`/`unbox-impl`/`constructor-impl` members for boxed contexts. The decision to compile value
  classes UNBOXED (not as plain single-field classes) is deliberate — a boxed-always shortcut miscompiles
  inline-class equality and identity (`X@hash` vs the value, `==` by reference), which a measurement
  confirmed (45 box FAILs); that is a test-hack, not the compiler kotlinc is. 886/0-FAIL. NEXT (step 4):
  member synthesis — emit `X.class` with kotlinc's exact members (field, private `<init>`,
  `constructor-impl`, `box-impl`, `unbox-impl`, getter, `equals`/`hashCode`/`toString` + `-impl` forms),
  verified by javap-diff vs kotlinc; then (step 5) use-site lowering: construction → `constructor-impl`,
  sole-property access on an unboxed value → identity, box/unbox only at nullable/generic/`Any` boundaries,
  mangled member names (phase 386). The resolve rejection + `ir_lower` `is_value` guards lift then.

- 🚧 **Phase 386 — value/inline classes, step 2: name mangling** (886, building block). New
  `src/jvm/inline_class.rs`: kotlinc's inline-class member-name mangling, ported exactly from
  `compiler/backend/.../inlineClassManglingUtils.kt` (new K2 rules). A function whose signature mentions
  a `value` class gets a `-<hash>` suffix where `<hash> = base64url_nopad(MD5(signature)[0..5])`; a value
  parameter contributes `L<fqName>[?];`, a mangled return contributes `:` + that element. Includes a
  small pure MD5 + URL-safe-base64 (no crypto dependency). Unit-tested against kotlinc 2.4.0 output:
  `value class S(val string)` → getter `getS-C-fiWsc` (return-mangled, `:LS;`), `fun useS(s: S)` →
  `useS-gSa4wCw` (param-mangled, `LS;`); top-level returns are NOT return-mangled (`mkS(): S` stays
  `mkS`). Pure utility, no compile-path wiring yet → 886/0-FAIL. NEXT (step 3+): value-class member
  synthesis (`box-impl`/`unbox-impl`/`constructor-impl`/getter) + underlying-type erasure + call-site
  routing through these names.

- 🚧 **Phase 385 — value/inline classes, step 1: corpus reaches the compiler** (886, scaffolding).
  The owner chose value/inline classes (~745 `inlineClasses/` box files) as the next frontier. The
  corpus files carry a literal `OPTIONAL_JVM_INLINE_ANNOTATION` placeholder line that the Kotlin test
  runner expands to `@JvmInline`; krusty's harness read raw source, so that bare identifier was the
  first parse error ("expected a top-level declaration") for every value-class file. The conformance
  harness now substitutes `OPTIONAL_JVM_INLINE_ANNOTATION` → `@JvmInline`, so these files reach the
  parser/checker (the parser already maps `value`/`inline` → `is_value`; the checker still rejects with
  "value/inline classes are not supported"). Behavior-preserving, 0-FAIL (still skipped, now at the
  checker not the parser). NEXT (step 2+): real unboxed codegen — generalize the existing UInt/ULong
  inline-class infra (`box_unsigned`/`unbox_unsigned`, `box-impl`/`unbox-impl`) to a user `value class
  X(val v: T)`: erase to the underlying `T` unboxed in non-nullable position, box to `X` when
  nullable/generic/`Any`, synthesize `box-impl`/`unbox-impl`, mangle use-site member names. Currently
  value classes are also excluded from the IR path (`ir_lower` `is_value` guards) — that gate moves as
  codegen lands. Diff against kotlinc per slice (equal-bytecode rule).

- ✅ **Phase 384 — synthetic-function registry: FQN → IR body** (886, refactor). New `src/synthetics.rs`:
  a simple registry mapping a compiler-**synthetic** function (one kotlinc realizes in codegen with no
  callable classpath body) to its **IR body**. It is the front end's **IR-level override** — during
  lowering a call is matched *before* classpath resolution (priority over the classpath; still shadowed
  by a user-declared same-name fn, the kotlinc rule) and the matched body contributes the call's IR
  directly. Each entry is `{ fqn, name, body }`; `body: fn(&Synthetic, &mut Lower, &SynthCall) ->
  Option<ExprId>` builds the IR with ordinary nodes (`Vararg`, `NewArray`, a fill loop via
  `Lower::build_fill_array`) and may *decline* (`None`) when it can't safely override (a branchy element,
  an undeterminable reified type). Bodies are emitted **inline at the callsite** by construction, so
  "inline" is not a stored attribute; element knowledge lives inside the array bodies (`prim_elem`), not
  the core struct, so the registry stays general. First family: the array creators (`arrayOf`,
  8× `*ArrayOf`, 8× `*Array(n)`/`*Array(n){}`, `Array(n){}`, `arrayOfNulls`, `emptyArray`); the inline
  fill-loop block + the `prim_array_elem`/`prim_array_of_elem` name tables moved out of `ir_lower`.
  The complementary **JVM intrinsic registry** (`jvm::ir_emit::emit_intrinsic`) is the **callsite
  bytecode override** — it realizes an IR `Call`/the single `NewArray` leaf as inline bytecode
  (`newarray int` for `Array<Int>`, `anewarray Integer` for `Array<Int?>`). Behavior-preserving, 0-FAIL.

- ✅ **Phase 383 — data-class array properties (proper support, replaces 382 skip)** (884→886). `ty_of`
  resolves array type names to `Ty::Array` (was `Any`), so array fields keep their `[I` type; data-class
  `toString` uses `Arrays.toString` (content) while `equals`/`hashCode` keep array reference identity —
  exactly kotlinc's behaviour. `tests/feature_box_e2e.rs::DataClassArray`.

- ✅ **Phase 382 — `ByteArray`/`ShortArray`/`FloatArray` constructors + data-class array-property skip**
  (878→884). Added the 3 missing primitive arrays to the checker's `primitive_array_element` (lowering
  already had all 8). Skip a data class with an array property (its erased-to-Object field + reference-
  semantics synthesized members would miscompile). `tests/feature_box_e2e.rs::MorePrimitiveArrays`.

- ✅ **Phase 381 — `as` to a primitive type (unbox cast)** (871→878). `x as Int` on a reference operand →
  `checkcast Integer; intValue()` (the emitter's existing `unbox_to`); checker allows a non-unsigned
  primitive target, lowering emits `ImplicitCoercion`. `tests/feature_box_e2e.rs::AsToPrimitive`.

- ✅ **Phase 380 — bridges with a primitive concrete type** (861→871). A getter/method bridge whose
  concrete member returns a primitive (generic `T` erased to `Object` overridden `: Int`) now boxes the
  primitive in the `ACC_BRIDGE` — the emitter already did this, so the over-conservative checker/lowering
  guards were removed. `tests/feature_box_e2e.rs::PrimitiveBridges`.

- ✅ **Phase 379 — property getter bridges (covariant / generic-erased overrides)** (856→861). A property
  overriding a supertype property with a different erased type gets a synthetic `ACC_BRIDGE` `getX()`
  returning the supertype's type, delegating to the concrete getter (reuses the method-bridge emit).
  `tests/feature_box_e2e.rs::PropertyGetterBridge`.

- ✅ **Phase 378 — `if`/`when` unrelated-reference branch join → common supertype (`Object`)** (849→856).
  Different reference classes join to `Any`; the emitter writes `Object` for the merge frame (each branch
  verifies as a subtype) and compares branch types by JVM internal name (so `Ty::String` vs
  `Ty::Obj("java/lang/String")` don't spuriously merge — that bug broke a both-`String` `if`).
  `tests/feature_box_e2e.rs::UnrelatedRefJoin`.

- ✅ **Phase 377 — `if`/`when` same-class branch join** (848→849). Two branches of the same class
  (`List<C>`/`List<D>`) join to that class (erased type args) — frame-safe since the runtime class is
  identical (the unrelated-class→`Any` join stays unsupported pending frame merging). `tests/feature_box_e2e.rs::SameClassJoin`.

- ✅ **Phase 376 — `super.method(args)` non-virtual dispatch** (845→848). New `Callee::Special` →
  `invokespecial` on `this` to the base method (skipping the override). Base method resolved from a user
  superclass (`method_of`) or a classpath one (`resolve_instance`), so `super.toString()` and a class
  extending a stdlib type work. Checker + lowering + emit + JS arm. `tests/feature_box_e2e.rs::SuperMethodCall`.

- ✅ **Phase 375 — `if`/`when` primitive+`null` branch join → boxed nullable wrapper** (843→845). A branch
  that is a primitive joined with `null` types as the boxed wrapper (`if (c) true else null` → `Boolean?`);
  the if/when lowering coerces each branch to a reference result type so the primitive branch is boxed at
  the merge (else a VerifyError). A broader two-references→`Any` join was reverted (frame-merge VerifyError).
  `tests/feature_box_e2e.rs::PrimitiveNullJoin`.

- ✅ **Phase 374 — unsigned range values + inline-class mangled-member resolution** (843, +0 capability).
  `0u..5u`→`UIntRange` (ctor with `DefaultConstructorMarker`), iterated via kotlinc's mangled getters
  (`getFirst-pVg5ArA`) — new `LibrarySet::mangled_member` looks the real name up from the classpath
  (superclass-chain walk), the first real inline-class infra. Unsigned counted loop uses `compareUnsigned`.
  `UByte`/`UShort`/open-ranges/`step` still unmodeled so corpus files stay skipped. `tests/feature_box_e2e.rs::UnsignedRangeIterate`.

- ✅ **Phase 373 — unsigned `in`-range membership + fast-iteration test profile** (843, +0 capability).
  `x in a..b` for `UInt`/`ULong` lowers to the bounds-check intrinsic with `compareUnsigned` (correct past
  the sign bit). Infra: added an unoptimized `[profile.gate]` (overflow-checks off) used by run-tests.sh
  by default — the in-loop round rebuilds in seconds and runs <60s without `--release`; conformance worker
  stack bumped to 64 MB so unoptimized recursion doesn't overflow. `tests/feature_box_e2e.rs::UnsignedInRange`.

- ✅ **Phase 372 — operator overloading via library functions + most-specific overload selection**
  (838→843). `a + b` on a reference receiver resolves `a.plus(b)` through the library set (`list + x` →
  `CollectionsKt.plus`). Required fixing extension-overload selection generally: subtype-aware candidate
  filter (`arg_fits_subtype`) + pick the most-specific overload, so `list + list` selects the `Iterable`
  concat overload, not the erased-`Object` element one. `tests/feature_box_e2e.rs::CollectionPlus`.

- ✅ **Phase 371 — test-suite speed (owner: round must be <60s)**. (a) The extension/top-level-function
  index is now shared process-wide via a path-keyed global cache (`global_ext_cache`), like the
  type/jimage indexes — the box harness's 16 workers stop each rebuilding it (check −2.7s thread-sum).
  (b) `feature_box_e2e` compiles snippets **in-process** through a shared `common::compile_in_process`
  helper (same `lex→check→lower→emit` pipeline as the conformance harness, warm caches) instead of
  spawning the krusty binary per snippet — that test dropped 24.5s→6s. Full validation round (gate + e2e
  + lib) execution is now ~29s. No behavior change; gate still 838/0-FAIL.

- ✅ **Phase 370 — direct `for` over `Byte`/`Short` range + step type coercion** (825→838). `Stmt::For`
  over `Byte`/`Short` operands widens to an `Int` counter (checker + lowering), and the loop `step` is
  coerced to the counter type (`0L..n step 3` adapts the `Int` step to `Long` — was a verify error).

- ✅ **Phase 369 — integer-family range widening + generic-vararg literal adaptation** (808→825).
  `Byte`/`Short`/`Int` range values → `IntRange`, a `Long` operand → `LongRange` (checker + lowering).
  `listOf<Long>(3)` adapts the int literal to a boxed `Long` via `LibraryCallable.vararg_elem` (only
  literals adapt — kotlinc semantics, no runtime `i2l`). `lower_foreach_range` made overflow-safe
  (break-before-increment) like `Stmt::For`, so a stored range ending at `Int.MAX_VALUE` doesn't spin.
  `tests/feature_box_e2e.rs::RangeWidenAndVararg`.

- ✅ **Phase 368 — a property reference is a function value** (`C::n` as `(C)->Int`). `KProperty1`/
  `KProperty0` accepted where a `Function1`/`Function0` of the matching arity is expected — in the checker
  (`expect_assignable`), the JVM library overload resolution (`arg_fits`, so `list.map(C::n)` works), and
  the lowering of a function-typed local (slot type from the annotation's `Ty::Fun`, so `f(arg)` invokes
  through `Function1.invoke`). Lowers to the existing `PropertyReference{1,0}Impl` — no new IR.
  `tests/feature_box_e2e.rs::PropertyRefFn`.

> Note: the next coverage levers (stdlib higher-order-function inlining for mutable-capture `forEach`/`map`;
> classpath companion-constants via `ConstantValue`; `UIntRange` value iteration with inline-class mangled
> getters; coroutines; inner classes; nullable primitives `Int?`) are each multi-file, infrastructure-scale
> efforts — see the coverage-roadmap notes for entry points. The 0-FAIL never-miscompile invariant holds.

## Bare-name stdlib hardcode audit (no-hardcode policy)  🚧

Standing rule: krusty may hardcode a value/desugar **only where kotlinc also intrinsifies it**; a
body-bearing stdlib function must be **inlined from its real bytecode** (the two-inliner architecture
below), not desugared by a hardcoded name. Every bare-name special-case in `ir_lower.rs`/`resolve.rs`,
classified:

**A. Receiver-TYPE-keyed member intrinsics — LEGITIMATE, keep** (a top-level name can't shadow them;
this is how every compiler does built-in member access). `enum .ordinal/.name/.values()/.valueOf()`,
`Char.code`, `Array.size`, `String.length`, `.equals/.hashCode/.toString`, and the unsigned/primitive
operator methods (`shl/shr/ushr/and/or/xor/inv/inc/dec/unaryMinus/unaryPlus`, `toUInt/toULong`) — all
genuine kotlinc backend intrinsics keyed on the operand type.

**B. Compiler INTRINSIC functions (no callable body in the stdlib) — keep, but RESOLUTION-GATE** so a
user function/local of the same name shadows them, exactly as kotlinc keys them on the resolved symbol:
`arrayOf`/`intArrayOf`/…/`IntArray(n)`/`emptyArray` ✅ gated (phases 312b + this); `Array(n){}` reference
bails (skip). `println`, `StringBuilder`/`Any` construction are type/library-resolved (low risk).

**C. Body-bearing stdlib INLINE functions desugared by name — VIOLATIONS to retire** (kotlinc inlines
their real `@InlineOnly` bytecode; krusty hardcodes an equivalent desugar). Verified by inspection of the
IR backend:
- `let`/`also` ✅ now route through the bytecode inliner (phase 310; desugar kept only as a this-capture
  fallback).
- **Still desugared in `ir_lower` (the real remaining violations):** `repeat` → counted `while`,
  `forEach`/`forEachIndexed` → for-each loop. Their stdlib bodies are *branchy* (a loop with the
  `FunctionN.invoke` inside it), so retiring the desugar needs the **branchy lambda-splice** (inliner
  step 2 below) — splicing the caller's lambda body at the invoke site *inside* a relocated branchy
  body. These are shadow-gated (no miscompile of a user fn) but remain hardcoded bodies.
- `run`/`with`/`apply` are **NOT desugared** — they bail (skip) in `ir_lower` ("not yet supported by the
  IR backend"); the old direct-AST emitter handled them (phase 55) but the IR backend never did. So they
  are an *unimplemented feature*, not a hardcode. Their bodies are *branchless* single-invoke (like
  let/also), so the cleanest implementation is to route them through the existing branchless inline route
  with this-receiver lambda lowering (receiver = the lambda's param 0) — a coverage gain done the
  rule-compliant way, no new desugar.

## Inline functions — the two-inliner architecture (mirrors kotlinc-JVM)

kotlinc-JVM inlines from whatever form the callee body exists in; krusty does the same with two
complementary inliners (decided after evaluating an IR-only approach — it cannot reach stdlib, whose
bodies exist only as jar bytecode):

- **Inliner #1 — IR inliner (same-module, user `inline fun`s).** ✅ Phases 285–286. Expands the body
  at each call site in the lowerer (`Lower::lower_inline_fn_call` / `lower_inline_lambda_invoke`):
  value params → once-evaluated temps; lambda args inlined at their function-typed parameter's invoke
  sites (no closure). Bails (file skipped, 0-FAIL) outside the subset (extension/reified/default/
  vararg/non-local-return) or on (mutual) recursion. This is K2's same-module path (body available as
  IR). Gap: the inline fn is not *also* emitted as a method, so the facade ABI differs (kotlinc emits
  it) — an ABI-parity gap, not behavioural.

- **Inliner #2 — bytecode splicer (cross-module stdlib `inline fun`s).** 🚧 The kotlinc-JVM path
  (`MethodInliner`): read the callee's compiled body from the classpath jar and splice it into the
  caller, relocating the constant pool. Retires the scattered `forEach`/`let`/`also`/`repeat` desugars
  (the no-hardcode win). `src/jvm/inline.rs` already has: `relocate_const`/`relocate_code` (pool
  relocation), `disassemble`/`assemble`, `shift_locals`, `redirect_returns`, `substitute_reified`,
  `param_store_ops`, and `splice()` wiring them — with unit tests.
  **Foundation DONE (phases 287–288):** the classpath is `Rc`-shared with the emitter inside the `jvm`
  module (no `LibrarySet` boundary); the emitter depends only on the narrow `MethodBodies` trait
  (`body(owner,name,desc)` — fetch bytecode by FQN, *not* the whole `Classpath`); `LibraryCallable`
  carries `is_inline` (decoded with the signature); the IR `Callee::Static` carries `inline: bool`; and
  the emitter routes an inline call to `Emitter::try_inline_static` (the splice decision point) with a
  hard fallback to `invokestatic`. Build order for the splice itself:
  **DONE:** branchless splice (phases 290–291); StackMapTable read (`MethodCode.stackmap`/`has_handlers`,
  292); `inline::decode_stackmap` (delta→absolute `Frame`s, unit-tested, 293).
  **Branchy splice — remaining integration (the hard sub-problems):**
  - **Offset remap after `shift_locals`.** Shifting locals by `base` grows instructions whose slot > 3
    (`iload_0`→`iload base`), so the body's byte layout changes. The decoded frame offsets (and every
    branch target) are byte offsets into the *original* layout → must be remapped old-byte-offset →
    instruction-index → new-byte-offset. `disassemble`/`assemble` already track instruction indices;
    expose the per-index old/new byte offsets to remap frames.
  - **Caller locals prefix.** A frame's locals must cover slots `0..base` (caller) then the body's
    locals. Reuse `Emitter::verif_locals` but a non-trimmed `0..base` variant; append the relocated
    body locals.
  - **Empty incoming stack only (first cut).** A frame's stack must be prefixed by the caller's operand
    stack at the splice point; krusty tracks stack *height* not *types*. So only splice branchy bodies
    when the baseline stack is empty (`cur_stack - arg_words == 0`: statement / `val x = f(...)`), else
    fall back. Sub-expression branchy inline calls stay on the call path.
  - **Type conversion + bail.** `VType::Object(cp)` → relocate the `Class` into `cw` → `VerifType::Object`;
    bail on `UninitThis`/`Uninit` (not modeled). The join-point frame (after the body's `goto end`):
    caller locals + the return value on the stack.
  - **Frame-add API.** Need to bind a label at an absolute byte offset within the appended body bytes
    (CodeBuilder.bind is "here"); add a `bind_at(label, offset)` or add frames keyed by absolute offset.
  Validate with a branchy kotlinc-lib e2e test (e.g. `inline fun atLeast(x,lo)=if(x<lo)lo else x`) +
  the conformance 0-FAIL gate (a botched frame → VerifyError → surfaces as a FAIL, so the gate catches it).
  1. **Branchless splice** through `try_inline_static`, behind the fallback (0-FAIL by construction).
     ⚠️ NOTE: `redirect_returns` rewrites even a single trailing `ireturn` into a `goto end`, which is a
     branch needing a StackMapTable frame — so the branchless path must instead *drop* the trailing
     return (single-exit body) to stay frame-free. Guard: branchless body (no branch opcodes), no
     exception table, no `Lkotlin/jvm/functions/Function` parameter. Add `CodeBuilder::splice_branchless`
     (append relocated bytes + stack/local bookkeeping) and `inline::is_branchless`. Test: compile a
     tiny lib with kotlinc that has a branchless `inline fun`, put it on krusty's `-cp`, assert krusty
     splices it (verifier-clean + correct runtime result).
  2. **Lambda-argument splicing (the crux).** Branchless + branchy *non-lambda* splice are DONE
     (290–295). The body calls `Function1.invoke(elem)` (invokeinterface); `inline::function_invoke_sites`
     (296, unit-tested) locates those sites. Two routes to handle the lambda parameter:
     - **(a) Closure route — tractable, high coverage, first cut.** Allow `Function`-typed params in the
       (already-built) branchy splice: emit the caller's lambda as a normal closure object and pass it as
       the action argument; the spliced body's `invoke` calls it. Needs: relax `param_vtypes` to admit
       reference params (frame-0 `Object(FunctionN)`); allow the `Lkotlin/jvm/functions/Function` guard in
       `try_inline_static`; thread the lambda arg through. Unlocks **all** stdlib lambda inline fns
       generically for **non-mutable-capture** lambdas (`run`/`with`/`apply`/`map`/`filter`/`fold`/…) — real
       coverage. NOT byte-equal to kotlinc (it inlines the lambda; we keep a closure), and **cannot**
       handle a lambda that writes an outer mutable local (the closure can't) — those keep the desugar.
     - **(b) True inline — retires the desugars fully.** At each invoke site, splice the caller's lambda
       *body* inline (bind its params to the invoke args), emitting krusty IR into the middle of the
       relocated stdlib bytecode. Removes the closure (matches kotlinc) and handles mutable capture, so the
       `forEach`/`let`/`also` desugars can be deleted. Hard: interleave IR emission with byte-splicing at
       `function_invoke_sites`, drop the dead `aload action`, and thread the lambda IR to the emitter.
     Plan: route (b) is the chosen path (owner: delete the desugars). EVIDENCE: krusty does NOT inline
     lambda inline fns today. A *regular* inline fn (`map`/`filter`/`fold`) is **called** — `map { it*2 }`
     → `invokestatic CollectionsKt.map(Iterable, Function1)` passing a closure (behaviorally correct but
     NOT inlined and NOT byte-equal: kotlinc emits the loop inline, no `CollectionsKt.map` call). An
     **@InlineOnly** fn (`let`/`also`/`run`/`apply`) is not callable from outside, so krusty desugars the
     few hardcoded ones (and bails on the rest). So route (b) is the path to **bytecode equality** for ALL
     lambda inline fns — the regular "called-not-inlined" ones too, not just @InlineOnly/mutable-capture.
     **Route (b) progress:** `function_invoke_sites` (296) locates the lambda calls; `branchless_lambda_segments`
     (297) prepares a branchless single-invoke body (`let`/`also`/`run`/`apply`) — relocate, shift locals,
     split at the invoke, elide the dead `aload <lambda-param>`, drop the trailing return → `(before, after)`
     instruction segments. **Emitter integration (next):** for an inline call with a lambda arg, emit the
     prologue storing only the *non-lambda* args (skip the lambda param slot); append `before` (relocated)
     bytes; emit the caller's lambda IR body inline (its params bound to whatever `before` left on the
     stack — store into the lambda's param slots, then `self.emit(lambda_body)`); append `after`; the value
     falls through. Captures (incl. mutable) resolve to the caller's own slots since the lambda IR emits in
     the caller's frame — which is exactly what the desugar achieves and the closure can't. REGRESSION
     GUARD: route only NOT-yet-desugared fns (`run`/`with`/`apply`/`takeIf`) through the splice first
     (additive, no regression), prove mutable capture works, THEN migrate `let`/`also`/`forEach` off the
     desugars and delete them. Branchy lambda fns (`forEach`/`map` loops) reuse the `splice_branchy` frame
     machinery with the invoke sites interleaved — after the branchless case works.
     **OBSTACLE (precise, traced phase 298):** the caller's lambda is already a *separate IR function* —
     `IrExpr::Lambda { impl_fn, captures, .. }`, `impl_fn` params = `[captures…, lambda_params…]` (see
     `lower_lambda_sam`). So "emit the lambda body inline" = **emit `impl_fn`'s body under a remapped
     value→slot environment** (capture indices → caller capture slots; param indices → fresh slots with
     the on-stack invoke args; `impl_fn` locals → fresh slots). krusty has **no "inline-emit an IR
     function body"** primitive — building `Emitter::emit_fn_body_inline(fid, slot_map)` is the core of
     route (b). Also: the checker must permit mutable capture for any inline-fn lambda arg (today only the
     named desugar set), since a by-value `impl_fn` capture param can't write the outer local — mutable
     capture needs the body emitted against the caller's *actual* slot, which inline-emit gives. Major
     multi-part feature; foundations (296–298) done.
     **EMITTER HALF DONE (phase 299):** `Emitter::emit_fn_body_inline` + `try_inline_lambda_call` inline a
     non-capturing lambda's body at a branchless single-invoke body's `FunctionN.invoke` (store non-lambda
     args, append `before`, unbox the boxed invoke args to the typed lambda params, inline the body, box
     the result, append `after`). 0-FAIL; reachable for any lambda-arg inline call (`map` → branchy → falls
     back). **TO FIRE — the ONE remaining front-end gap (precisely diagnosed):** custom-lib top-level fns
     DO resolve (`dbl(5)` works — the earlier "unresolved" was a stdin-facade test artifact; a named
     `Lib.kt`→`LibKt` resolves). The real gap: the resolver types a top-level lib fn's **lambda parameter**
     from the *erased* descriptor (`Function1`), so `applyIt(5){ it+1 }` gives `it: Any` + "type mismatch:
     Function vs Function1". FIX: parse the lib fn's generic `Signature` (jvm_libraries has
     `parse_method_gsig`) → the `Function` param's `(Int)->Int` → a `lambda_param_types` on `LibraryCallable`
     → `resolve.rs` (~3597 arg loop) types the lambda with it (as the user-fn `known_sig` path does). THEN
     `applyIt` lowers to `Callee::Static(inline)` + lambda → the phase-299 emitter inlines it → route (b)
     fires end-to-end via a custom lib (no stdlib @InlineOnly/multifile complications).
     (ii) stdlib `let`/`also` are IR-desugared (`ir_lower` ~3261, a clean IR true-inline) — route only
     *non-capturing* ones to `Callee::Static(inline)` (keep the desugar for capturing lambdas; detect free
     vars at the desugar site). (iii) an inlined `impl_fn` is still emitted as a dead method — skip it for
     byte-equality.
     **ROUTE (b) FIRES (phases 299–302):** krusty truly inlines a cross-module lambda inline fn end-to-end
     (`applyIt(5){it+1}` → inlined, no call, verifier-clean, 0-FAIL). The engine = `emit_fn_body_inline` +
     `try_inline_lambda_call` (emitter) + `toplevel_lambda_param_types` (resolver types `it` from the
     generic Signature) + `checkNotNullParameter`-strip + body-slot reservation. v1: branchless single-
     invoke, non-capturing, single-value lambda; proven on a single-file-facade custom lib.
     **TO RETIRE THE STDLIB DESUGARS (`let`/`also`/`forEach`) — the next arc, each a real sub-step:**
     (a) **Multifile-facade body read.** `let`'s body is in `kotlin/StandardKt__StandardKt.class` (the part),
     NOT the facade `StandardKt.class` (a 413-byte stub) — `MethodBodies::body(facade,…)` returns None.
     Fix: when the facade lacks the method, read from its multifile parts (the facade's `@Metadata` d1 lists
     them, or scan classpath for `{facade}__*`). Gateway to all stdlib scope/collection inline fns.
     (b) **Route off the desugar.** In `ir_lower` (~3261) route a *non-capturing* `let`/`also` to
     `Callee::Static(inline)` (keep the desugar for capturing lambdas; detect free vars at the desugar site).
     (c) **Captures** — `forEach { s += it }`: bind the lambda impl's capture params to the caller's slots
     (mutable capture works since the body emits in the caller frame). (d) **Unit lambdas** (`also`/`forEach`)
     — the v1 guard rejects them; emit the Unit result. (e) **Branchy bodies** (`forEach`/`map` loops) —
     interleave the lambda at `function_invoke_sites` inside the `splice_branchy` frame machinery.
     (f) **Receiver-rebind** (`run`/`with`/`apply`: `this` not `it`). (g) skip emitting the now-dead inlined
     `impl_fn` method for byte-equality.
     **DELETING THE `let`/`also` DESUGAR — the precise blocker chain (diagnosed phase 308, all in the
     front end):** the inliner ENGINE is complete (route b inlines any lambda shape — value/Unit/captures/
     mutable/non-local-return — proven on custom-lib fns), but stdlib `let`/`also` can't be *routed* to it:
     (1) ✅ body lives in the multifile part `StandardKt__StandardKt` — `method_code` reads it (303).
     (2) the ext index excludes `let`/`also` because they're `static` but **non-public** (`@InlineOnly`
     makes them package-private to block Java calls) — `collect_class_bytes` filters `is_public`; must
     include non-public statics (gated by the `inline` flag at the call site so non-inline non-public
     methods aren't emitted as broken calls). (3) THE REAL BLOCKER: even with (2), the **checker types
     `let`'s lambda argument as `Ty::Error`** in `TypeInfo` (it relies on the name-matched `let`/`also`
     handling in `resolve.rs` and never records the lambda arg's `Ty::Fun`), so `resolve_callable`'s
     `arg_fits(Function1, Error)` fails → the route can't resolve it. Fix order: make the checker resolve
     `let`/`also` via the library (recording the lambda arg as `Ty::Fun`) + index non-public statics; then
     `try_route_lambda_inline` resolves them, the inliner splices, and the desugar deletes (0 coverage
     loss — the engine handles every shape, verified phase 307). This is a front-end (resolver) arc, not
     an inliner one.
     **FULLY MAPPED (phase 309 — got it working end-to-end, then reverted on a 0-FAIL regression):** the
     complete fix chain, all verified individually correct, is: (a) `method_code`/`is_inline_method` follow
     the **superclass chain** (a multifile facade *extends* its parts: `StandardKt` → `StandardKt__Synchron…`
     → `StandardKt__StandardKt`; d2 is empty — the earlier d2 approach never fired); (b) the checker's
     `let`/`also` handler uses `check_lambda_with_types` so the lambda arg's `Ty::Fun` is RECORDED in
     `TypeInfo` (was `Ty::Error`); (c) the ext index includes **non-public** statics (`@InlineOnly` scope
     fns are package-private); (d) the prologue **boxes** a primitive receiver into the `Object` param
     (`5.let{…}`); (e) the route wraps the call in `coerce_erased(ret, physical_ret)` to unbox the erased
     `Object` result to the logical type. With (a)–(e), `let`/`also` inline correctly for value/Unit/
     capture/mutable/chained/non-local-return (all verified). **DONE (phase 310):** the public/non-public
     split shipped. Each `ExtCandidate` carries a `public` flag; the ext index includes non-public
     (`@InlineOnly`) statics but **every normal-resolution consumer filters to public-only** —
     `resolve_callable` (receiver, top-level, and `$default` paths) and `extension_lambda_param_types`.
     Only `resolve_scope_inline` (the inline route) reads non-public, and it emits no call (it splices),
     so there is no `IllegalAccessError` exposure. The route (`try_route_lambda_inline`) is wired into the
     `Expr::Member` arm: any library `inline fun` taking a single closure-form lambda the platform can
     splice is inlined from its REAL stdlib bytecode (verified: `5.let{it+1}` emits the spliced
     `StandardKt.let` body — `Integer.valueOf`/`checkcast`/`intValue` round-trip — not a desugar).
     Conformance holds **476 box()=OK / 0 FAIL** (full parity, no regression). A per-function `let`/`also`
     desugar is KEPT only as a fallback for lambdas that capture `this`/fields (no closure form ⇒ no
     `IrExpr::Lambda` to splice); removing it costs ~13 box tests until this-capturing lambdas are
     modelled, so it stays until the route covers them.
  3. **Non-local return** from an inlined lambda (`return` in `list.forEach { return ... }`): map to a
     jump out of the enclosing function (kotlinc uses a generated finally/label). Until done, bail.
  4. **invokedynamic relocation** (bootstrap-method + method-handle pool entries) — `relocate_const`
     bails on these today; needed when a spliced body itself constructs a lambda.
  Invariant throughout: any unhandled construct ⇒ fall back to the existing (working) call/desugar path;
  never emit unverified bytecode. Validate each step against the box conformance gate (0 FAIL) plus a
  byte-diff vs kotlinc for the spliced method.

### Phase 438 — loop-host lambda-frame context infrastructure; isolate the operand-baseline frontier  ✅
- `splice_unified` now reports `lambda_host_locals`: the host's LIVE body locals at each lambda's invoke
  (the loop-body frame for a loop host — iterator/accumulator — not just the parameters), falling back to
  the parameters when no host frame precedes the invoke (`takeIf`). `merge_lambda_frame_locals` uses this
  as the host context for a spliced lambda body's frames, so a branchy lambda body inside a loop gets the
  correct live-local context.
- This isolated the LAST blocker for loop-host inlining: `map`/`fold`/`forEach` build a collection, so the
  lambda is invoked at a NON-EMPTY operand baseline (the destination sits on the stack for the later
  `.add`) — the lambda body's own frames need that operand-stack PREFIX, which requires abstract-
  interpreting the host's operand stack at the invoke (a further subsystem). Until then, a lambda host
  with a loop bails: a PUBLIC one (`map`/`fold`/`forEach`) falls back to a real call, a non-public one
  skips — never a miscompile.
- Box gate **1313, 0 FAIL** (unchanged; `feature_box_e2e::TakeIf` green). Empty-operand-baseline hosts
  (`takeIf`/`takeUnless`/`require`/`check`/`let`/`also`/`run`/`apply`) inline fully, branchy host and
  branchy lambda body included. Loop hosts (non-empty baseline) are the remaining inline frontier.

### Phase 436 — inline branchy-lambda hosts (`takeIf`/`takeUnless`): full host + lambda-body frame relocation  ✅
- `splice_unified` now relocates a BRANCHY lambda BODY's own `StackMapTable` frames (a comparison
  predicate `{ it.length == 2 }` materializes to branches), not just the host's. The emitter
  (`try_inline_unified`) builds each lambda body to a scratch `CodeBuilder`, LINKS it (patches its branch
  operands), captures its resolved frames, and binds them at `lambda_byte_start + frame_offset` with full
  locals = caller prefix + host params (`merge_lambda_frame_locals`). `splice_unified` reports each
  lambda body's absolute byte start; the repl's internal branch targets are shifted by its merged position.
- `assemble`/`insn_offsets` gained `_at(base)` variants so a spliced body containing a `tableswitch`/
  `lookupswitch` (`toList`'s `when (size)`, `listOf`) pads to 4 bytes from the REAL method offset — the
  branchy path re-splices at its `splice_start`. Fixed `typeAliasAsBareType` (a `tableswitch` corrupted by
  0-based padding).
- `can_inline_lambda` opened to a `splice_unified` dry-run (any host it can splice). Guard against a
  value-class extension (`Result.map`, receiver erased to `Object`) wrongly matching an unrelated receiver
  through the erased `Object` key: a non-public extension matched via that key must have a TYPE-VARIABLE
  receiver (`T.takeIf` — the scope-fn family). Checker types the predicate via `extension_lambda_param_types`
  (now non-public + non-`Obj`-receiver aware) and the return via `resolve_scope_inline`.
- Box gate **1311 → 1313, 0 FAIL** (TDD `feature_box_e2e::TakeIf`: `takeIf`/`takeUnless` with comparison
  predicates, JVM `-Xverify:all`). REMAINING (next): LOOP hosts (`map`/`fold`/`forEach`) — their loop
  back-edge frames aren't relocated yet, so a lambda LOOP host still bails (a public one falls back to a
  real call, a non-public one skips — never a miscompile). That guard is the last one to remove.

### Phase 435 — splicer hardening toward complete inline support (branchy-lambda hosts)  ✅
- Toward inlining branchy-lambda hosts (`takeIf`/`takeUnless`): attempted relaxing the `can_inline_lambda`
  gate to a `splice_unified` dry-run so any classpath lambda host routes through the one splicer. The
  conformance gate caught that `splice_unified` is NOT yet robust for arbitrary hosts — 3 complex stdlib
  HOFs miscompiled (`capturedLoopVar`: a `Function0.invoke` left on an `ArrayList`; `kt20844`/
  `typeAliasAsBareType`: `VerifyError`) and a non-lambda reference param mis-spliced. A structural dry-run
  can't prove frame/operand correctness, so the relaxation was reverted to preserve never-miscompile.
- KEPT (defensive, so a future relaxation is safer): `splice_unified` now bails a lambda host with a LOOP
  (backward branch) or with an invoke-count ≠ lambda-count (ambiguous pairing); `try_inline_unified`
  rejects a branchy lambda BODY via `CodeBuilder::has_frames()` (the `needs_stackmap` flag missed
  `emit_compare`'s frames); scope fns (`let`/`also`) lower with `must_inline: true` (non-public — a failed
  splice skips, never an `IllegalAccessError`). Box gate **1311, 0 FAIL** (unchanged).
- REMAINING for complete inline support: make `splice_unified` relocate a branchy lambda body's frames
  (a materialized comparison predicate) and handle loop hosts + non-lambda reference params correctly;
  then the gate can safely open to `takeIf`/`takeUnless`/`mapIndexed`/… Also: primitive-receiver `takeIf`
  needs `T?` (`Int?`) nullability.

### Phase 434 — universal splicer: N-ary lambdas; one `splice_unified` for every inline shape  ✅
- Final merge step. `splice_unified` now handles N-ary (`FunctionN`) lambdas, not just `Function0`: a
  lambda's `aload` is no longer required adjacent to its `invoke` (the argument expressions sit between),
  so it locates the single lambda-object load (ignoring the entry null-check's load) and the
  `FunctionN.invoke` after it, then DELETES the load and REPLACES the invoke with the lambda body. The
  emitter (`try_inline_unified`) builds that body to consume the on-stack `Object` arguments — unbox each
  to its typed parameter and store it (a reference keeps its precise verification type, no checkcast),
  run the body, box the result. Branches on `join_required`: a branchless host (`let`/`also`/`run`/
  `apply`) splices at ANY operand-stack height (mid-expression); a branchy host needs an empty baseline.
- `let`/`also` are now routed through `splice_unified` (with `must_inline: true`, since they're non-public
  `@InlineOnly` — a failed splice skips the file, never an `IllegalAccessError`). DELETED the last special
  splicers + their dead helpers: `branchless_lambda_segments`, `try_inline_lambda_call`, `append_segment`.
  Only `splice` (reified-type substitution, a distinct `reifiedOperationMarker` concern) remains separate.
- Box gate **1311, 0 FAIL** (pure refactor; conformance-verified, `ScopeFns`/`ScopeFnsBranchy`/
  `RequireCheckMsg` e2e green). The bytecode splicer is now ONE function. NEXT (complete inline support):
  relax the `is_lambda_spliceable`/`can_inline_call` gates to a `splice_unified` dry-run so branchy-lambda
  hosts (`takeIf`/`takeUnless`) inline too, then retire the gates.

### Phase 433 — merge `splice_branchless` into `splice_unified` (the no-lambda splicer is now one fn)  ✅
- `splice_unified` now DROPS a trailing return (fall through with the result) instead of `goto`-ing it to
  the join, and reports `join_required`: `false` for a pure branchless body (no branches, single trailing
  return) — which the emitter then appends at ANY operand-stack height (mid-expression), exactly like the
  old `splice_branchless`; `true` for a branchy body (needs the join frame + an empty baseline). Added a
  guard: a branchy body with no `StackMapTable` bails (can't synthesize the target frames).
- The emitter's `try_inline_static` now calls `splice_unified` ONCE and branches on `join_required`;
  `can_inline_call` is a single `splice_unified` dry-run. DELETED `splice_branchless` + `is_call_spliceable`
  (its unit tests migrated to `splice_unified`). Remaining: `branchless_lambda_segments` (the `let`/`also`
  receiver-lambda path — needs N-ary `FunctionN` support in `splice_unified`; v1 is `Function0`).
- Box gate **1311, 0 FAIL** (pure refactor; conformance-test verified). Two splicers were three; now the
  no-lambda + branchy + `Function0`-lambda cases are ONE `splice_unified`.

### Phase 432 — merge `splice_branchy` into `splice_unified` (one splicer for branchy bodies)  ✅
- First step of unifying the bytecode splicers: `splice_unified` with NO lambda arguments subsumes the
  old `splice_branchy` (same `BranchySplice` result — relocated frames + join). Routed the emitter's
  branchy path (`try_inline_static`) and the `can_inline_call` branchy dry-run through it, then DELETED
  `splice_branchy` and its now-unused `param_vtypes` helper. `splice_unified`'s `param_vtypes_full`
  (reference param → `Top`, guarded to lambda-only) replaces the primitive-only `param_vtypes`.
- Box gate **1311, 0 FAIL** (unchanged — pure refactor, verified by the conformance test, not just the
  OK-count print). Remaining splicers: `splice_branchless` (needs `splice_unified` to drop a trailing
  return / skip the join frame for a fall-through body, to avoid extra `goto`/frame) and
  `branchless_lambda_segments` (needs N-ary `FunctionN` lambda support in `splice_unified`; v1 is
  `Function0`). Those two land next, then the splicer is truly one.

### Phase 431 — unified host+lambda inline splice: `require(cond) { lazyMessage }` / `check(cond) { … }`  ✅
- The two-arg precondition overload: a BRANCHY host body (`if (!cond) throw IAE(lazyMessage())`) that
  invokes a lambda PARAMETER only on the failure branch. Neither `splice_branchy` (no lambda) nor
  `branchless_lambda_segments` (no branches) handled it. New `splice_unified` (jvm/inline.rs) is the merge:
  it relocates a possibly-branchy host in the instruction domain and replaces each zero-arg
  `Function0.invoke` site with that lambda's pre-built body, remapping branch targets and StackMapTable
  frames over the edits (null-check strip + lambda insert) and dropping the spliced-away lambda slot
  (dead → `Top`). `try_inline_unified` (jvm/ir_emit.rs) drives it: emits each lambda body to a scratch
  builder (captures bound to caller slots → a mutable capture writes through), then splices.
- Resolution: `can_inline_call` now also dry-runs `splice_unified` for a host with `Function0` params, so
  `require`/`check`'s two-arg overload resolves (non-public `@InlineOnly`, `must_inline` → splice-or-skip).
  The `$default` "trailing lambda" guard no longer blocks the non-public branch.
- Two checker fixes this enabled: (1) a top-level `must_inline` callee's lambda is typed with mutation
  allowed (an inline capture, not a `Ref`); (2) `stmt_refs_param` now counts an `Assign`/`+=` TARGET name,
  so a WRITE-ONLY captured var (`require(false) { ran = true }`) is detected as a capture (was missed →
  unresolved in the lambda body). v1: zero-arg (`Function0`) lambdas with branchless bodies.
- Two regressions the change exposed, both FIXED (never-miscompile held by the conformance gate):
  (a) `assert` is a codegen INTRINSIC (guarded by a synthetic `$assertionsDisabled` / `ASSERTIONS_MODE`,
  arg ELIDED when disabled) — splicing its library body (`kotlin/_Assertions.ENABLED`) reproduces neither,
  so `splice_unified` now refuses any `_Assertions`-referencing body (assert stays skipped, as before).
  (b) `stmt_refs_param` counting an `Assign` target un-skipped `inlineClassValueCapturedInNonInlineLambda`,
  which VerifyError'd: a `@JvmInline value class` var is UNBOXED, so a captured-and-written one must box
  into its UNDERLYING type's `Ref` (`Z(Int)` → `Ref.IntRef`), not `Ref.ObjectRef` — fixed in `lower`.
- Box gate **1307 → 1311, 0 FAIL** (TDD `feature_box_e2e::RequireCheckMsg`: lambda runs only on failure,
  mutates an outer `var`, JVM `-Xverify:all`; plus the value-class capture test now passes). NEXT: route
  `splice_branchless`/`splice_branchy`/`branchless_lambda_segments` through `splice_unified` and delete
  them (finish the merge); N-ary lambdas.

### Phase 430 — delete the hardcoded precondition/intrinsic checker (`require`/`check`/`error`/`TODO`)  ✅
- `check_precondition_intrinsic` name-matched `require`/`check`/`assert`/`error`/`TODO`/`assertEquals`/
  `assertTrue`/`assertFalse` and hardcoded their return types + argument validation — a reimplementation
  of stdlib signatures the project forbids. Deleted entirely. These are now resolved generically through
  the library set from their real classpath descriptor (`(Boolean[, () -> Any]) -> Unit`,
  `(Any) -> Nothing`, `() -> Nothing`, …) and spliced from compiled bytecode, like any other call.
- A missing stdlib now simply leaves the call unresolved (it doesn't type-check) instead of needing the
  bespoke `'TODO' requires the kotlin stdlib` presence check.
- Box gate **1303 → 1307, 0 FAIL**: generic resolution covers strictly MORE forms than the hardcode did
  (extra message/overload variants), so removing the special-case raised coverage. No regression for the
  `kotlin.test` asserts. Open follow-up: two-arg `require(cond) { lazyMessage }` (a branchy host that
  invokes a lambda parameter) needs the unified host+lambda splice.

### Phase 429 — branchy splicing of non-public `@InlineOnly` preconditions (`require`/`check`)  ✅
- `require(cond)` / `check(cond)` are NON-public (`@InlineOnly`) `inline fun`s with BRANCHY bodies
  (`if (!cond) throw IllegalArgumentException("Failed requirement.")`). kotlinc emits no callable method,
  so there is no legal `invokestatic` — they MUST be inlined. Previously `can_inline_call` accepted only
  *branchless* bodies, so `resolve_callable` left them unresolved → the file skipped. Now the gate also
  dry-runs `splice_branchy`, and the emitter (already wired for branchy splicing) relocates their
  StackMapTable frames at the call site.
- **Never-miscompile guard:** a non-public callee that the emitter can't splice (a branchy body on a
  NON-empty operand stack needs an operand-stack prefix krusty can't yet supply) has no fallback. New
  `LibraryCallable.must_inline` / `Callee::Static.must_inline` mark such calls; if the splice fails the
  emitter sets a thread-local bail and `emit_all` returns `None` (skip the file) — never an
  `IllegalAccessError` from an `invokestatic` on the private method.
- **Bug found + fixed (operand-stack drift):** a materialized primitive comparison (`1 + 1 == 2` as a
  *value*) left `CodeBuilder.cur_stack` one slot too high — `bind(t)` didn't reset the linear height to
  the branch-point height, so the fall-through's `push 0` carried past the `goto`. Harmless for
  `max_stack`, but it made `stack_height()` over-report, which the branchy-inline baseline check relies
  on (a following statement then saw a phantom non-empty stack and bailed). Also: a `)V` (void) return is
  now 0 words in the inline splice (was `Unit` = 1), so a spliced void body leaves the stack balanced.
- Box gate **1303, 0 FAIL** (TDD `feature_box_e2e::RequireCheck`: `require`/`check` pass-through +
  `IllegalArgumentException`/`IllegalStateException` thrown-and-caught, JVM `-Xverify:all`). Still not in:
  the two-arg `require(cond) { lazyMessage }` lambda overload (branchy + lambda splice) — a follow-up.

### Phase 428 — user generic `inline fun` HOFs: bind a return-only type param from the lambda's return  ✅
- The follow-up left open by phase 427: `applyFn<T, R>(x: T, f: (T) -> R): R` called `applyFn("ab") { it.length }`
  failed (`VerifyError: Bad type on operand stack … astore`). `R` is bound by neither a value arg nor a
  lambda *parameter* — only by the lambda's RETURN type (`it.length` → `Int`). The checker still typed the
  call `Any`, so the `Int` result was `astore`d into a reference slot → verify mismatch.
- New `user_generic_return(fname, arg_tys)` runs AFTER the lambda args are typed (unlike `user_generic_call`,
  which produces the lambda parameter types and so must run before). It binds ALL type params from the full
  argument types — value args, and for a function-typed parameter `(A) -> R` the actual `Ty::Fun` supplies
  `A` (from `params`) and `R` (from `ret`) — then substitutes the declared return type. `user_generic_call`
  now returns only the lambda parameter types; the call's specialized return comes from the new method.
- Box gate **1303, 0 FAIL** (TDD `feature_box_e2e::GenericInlineHof` extended: `applyFn("ab"){it.length}`==2,
  alongside the existing `twice` cases). `stringGeneric.kt` stays skipped (a non-inline generic, never FAIL).

### Phase 427 — user generic `inline fun` HOFs: specialize type params from value arguments  ✅
- A user `inline fun <T> twice(x: T, f: (T) -> T): T = f(f(x))` called `twice(1) { it + 10 }` failed: the
  lambda's `it` typed as the erased `Any` (`it + 10` → "operator on Any and Int"), and even after that the
  call's return was `Any` (`Nothing`-vs-value mismatch → VerifyError). The IR inliner also bailed on any
  generic inline fn. Now the inliner SPECIALIZES the (non-reified) type parameters from the call's VALUE
  arguments — a parameter/lambda/return declared `T` takes the concrete argument type (`Int`/`String`).
- Checker: `user_generic_call` finds the user FunDecl, binds its type params from the typed non-lambda
  arguments, and reports the lambda parameter types AND the specialized return type. The receiver-less
  lambda-typing guard is broadened to any user fn (so a generic inline HOF reaches it); the library
  lambda-typing path stays gated on `known_sig.is_none()` so a user fn still shadows a library one. Only
  `is_inline` generic functions specialize — a NON-inline generic fn runs through the erased `Function1`
  (its lambda `it` is `Object` at runtime), so specializing it would mismatch and break value-class args.
- Lowering (`lower_inline_fn_call`): the `!type_params.is_empty()` bail relaxes to `!reified_type_params`;
  value-parameter slots and lambda-parameter types use the type-param→actual-arg bindings (`tbinds`), so
  the spliced body sees `Int` and avoids spurious boxing.
- Box gate **1303, 0 FAIL** (TDD `feature_box_e2e::GenericInlineHof`: `twice(1){it+10}`==21, `twice("x")
  {it+"!"}`=="x!!"). Not yet: a type param bound only by a lambda's RETURN (`<T,R> (T)->R`) — a follow-up.

### Phase 426 — inline `@InlineOnly` diverging stdlib functions (`error`) via the splicer  ✅
- `error(msg)` (= `throw IllegalStateException(msg.toString())`) is a real `@kotlin.internal.InlineOnly`
  stdlib function: kotlinc emits no callable method, so it MUST be inlined. It was unsupported (the call
  bailed) — NOT hardcoded. Now it is DISCOVERED from `@Metadata` (`is_inline`) and SPLICED from its real
  jar bytecode, like any inline function — no reimplemented body.
- Splicer: `splice_branchless`/`is_call_spliceable` now accept a DIVERGING branchless body — one ending in
  `athrow` with no `return` (a `Nothing` function) — splicing it whole (control never falls through). The
  splice leaves nothing on the stack, so `try_inline_static` uses `ret_words = 0` for a diverging body.
- Resolution: `resolve_callable`, after no public/`$default` match, resolves a NON-public `@InlineOnly`
  top-level function as `is_inline` (gated by `can_inline_call`, which dry-runs the splice — so an
  un-spliceable body stays unresolved rather than falling back to an `invokestatic` on the private method).
- Divergence in emit: a `Static`/`Virtual` call whose JVM return descriptor is `Ljava/lang/Void;` (kotlin
  `Nothing`) reports `value_ty = Nothing`; `diverges()` treats a `Nothing`-typed call as non-falling-through;
  `emit_when` skips the value-`discard`/merge-frame for a diverging branch. So an inlined `error(...)` works
  in linear, `if`-branch, and `try` positions.
- Also skip (sound) `inlineClasses/overrideReturnNothing`: a covariant override returning `Nothing` lowers
  to a `java/lang/Void` bridge return that the bridge emitter can't `areturn` — `lower_file` bails the file.
- Box gate **1303, 0 FAIL** (+6). TDD: `feature_box_e2e::ErrorInline`. Toward the goal (owner): a COMPLETE
  splicer that inlines every inline function, eventually removing the `can_inline_call` feasibility gate.

### Phase 425 — top-level function overloading  ✅
- `fun f(Int)` / `fun f(String)` (same name, different parameter signatures) used to error as "conflicting
  declarations" — krusty's symbol table was name-keyed (`funs: HashMap<String, Signature>`,
  `fun_ids: HashMap<String, u32>`). Now a name holds ALL its overloads and a call selects one by argument
  types; each overload emits as its own JVM method (same name, different descriptor).
- `funs` is `HashMap<String, Vec<Signature>>`; `fun_ids` is keyed by `(name, erased-param-descriptor)`.
  Collection keeps every same-name function, rejecting only an EXACT erased-parameter duplicate (a real
  `ClassFormatError`). A shared `pick_overload(sigs, arg_tys)` — used identically by the checker and the
  lowerer so they always agree — filters by arity (varargs/defaults aware) then scores by argument fit.
- Soundness guards (skip rather than miscompile): (1) krusty erases generics, so a generic value reads as
  `kotlin/Any`; if an argument is the erased `Any` where candidate parameter types DIFFER, `pick_overload`
  returns `None` (kotlinc would select on the precise type krusty lost) and the call is left unresolved.
  (2) Member (class-method) overloading stays rejected — it needs erasure/bridge handling krusty doesn't
  model (`check_no_erased_clash(..., allow_overload=false)` for members, `true` for top-level). (3) A
  cross-file function (in `funs` but not this file's `fun_ids`) falls through to the facade-call path.
  `::foo` and lambda-arg pre-typing resolve only for an unambiguous (single-overload) name.
- Box gate **1297, 0 FAIL** (+3). TDD: `feature_box_e2e::FunctionOverloading` (type- and arity-distinguished,
  plus the ordering-sensitive `(Int,Any)` vs `(Any,Int)` case). Cross-file dropin e2e kept green.

### Phase 424 — `is`/`!is` with a nullable reference target (`x is A?`)  ✅
- `x is A?` includes `null` (`null is A?` is true), but a bare `instanceof` is false for null, so krusty
  rejected any nullable `is` target. Now a nullable REFERENCE target lowers to `x == null || x is A`
  (and `x !is A?` to the De Morgan dual `x != null && x !is A`), binding the operand to a temp so it is
  evaluated once. A nullable PRIMITIVE target (`x is Int?`) stays rejected (box/unbox semantics).
- Checker (`Expr::Is`): the nullable rejection now only fires for a non-reference target. Lowering:
  resolves the target as its non-null base (`ty_ref` returns `None` for any nullable type), builds the
  `RefEq`/`InstanceOf` (or `RefNe`/`NotInstanceOf`) pair joined by `Or`/`And` over an `Object`-typed temp
  (a precise operand type — or `null`/`Nothing` — would be an invalid local-variable type).
- Box gate **1294, 0 FAIL** (+3, incl. `basics/check_type.kt`, `typeMapping/nullNothing.kt`). TDD:
  `feature_box_e2e::IsNullableType`.

### Phase 423 — `Unit` as a value + `Unit`-returning covariant-override bridge  ✅
- `Unit` used as an expression (`foo(Unit)`, `val u = Unit`, `return Unit`) is the `kotlin/Unit` singleton,
  not a type. krusty rejected the bare identifier ("unresolved reference 'Unit'"). Now the checker's
  `Expr::Name` resolution has a final fallback (after locals/properties/objects, so any user `Unit` still
  wins): `Unit` → `Ty::obj("kotlin/Unit")`; lowering emits the existing `IrExpr::UnitInstance`
  (`getstatic kotlin/Unit.INSTANCE`). `value_ty(UnitInstance)` now reports `kotlin/Unit` (was the `Ty::Error`
  default). `u.toString()` is "kotlin.Unit"; the singleton compares equal/identical to itself.
- Exposed + fixed a latent bridge bug: a `Unit`-returning override of a reference-returning supertype
  method (`B.foo(): Unit` over `A.foo(): Any`) emits a bridge `foo()Ljava/lang/Object;` that invokes the
  void `foo()V` then `areturn` — with nothing on the stack (operand-stack underflow). `Unit` is not a
  primitive, so the bridge's box path skipped it. `emit_bridges` now materializes `kotlin/Unit.INSTANCE`
  after the void call when the concrete return is `Unit` and the erased return is a reference.
- Box gate **1291, 0 FAIL** (+6, incl. `bridges/test18.kt`). TDD: `feature_box_e2e::UnitAsValue`.
- Not covered (future): materializing `Unit` from a *void call used as a value* (`val x = foo()` where
  `foo(): Unit`) — the void→value duality at arbitrary call sites; only the explicit `Unit` literal and the
  override-bridge return are handled here.

### Phase 422 — Kotlin-type-aware collection `+=` (read-only/mutable), the way kotlinc does it  ✅
- Goal: `coll += x` mutates in place for a mutable collection but reassigns (`coll = coll.plus(x)`) for a
  read-only one — decided exactly as kotlinc, with NO mutability predicate and NO hardcoded hierarchy, and
  with type erasure happening ONLY at emit. The read-only/mutable identity exists in no JVM descriptor
  (`List` and `MutableList` both erase to `java/util/List`); it lives in `@Metadata` and `.kotlin_builtins`.
- Front-end types flipped to Kotlin: `resolve_callable` returns `kotlin/collections/{List,MutableList,…}`
  (return type from `@Metadata`, `meta_collection_ret`); `ir_lower::ty_of` and the resolver seed use
  `kotlin_builtin_to_internal` (keeps `List` vs `MutableList`). `to_jvm_internal` erases both to
  `java/util/*` at the bytecode boundary (phase 420). So `mutableListOf()` is a `MutableList`, `listOf()` a
  read-only `List`, through the whole front end.
- The Kotlin collection hierarchy (`MutableList : List, MutableCollection`) is READ from
  `kotlin/collections/collections.kotlin_builtins` on the classpath — a `BuiltInsBinaryVersion` header +
  `PackageFragment` proto, resolved through its `QualifiedNameTable`/`StringTable` exactly as kotlinc's
  `NameResolverImpl` (`metadata::builtins_supertypes`; `Class.supertype_id` → `type_table` →
  `Type.class_name`). NOT hardcoded.
- Resolution is Kotlin-type-aware, generically (kotlinc has no `is_mutable_collection`): `+=` resolves a
  `plusAssign` operator candidate; `extension_callable` rejects a candidate whose Kotlin receiver (decoded
  from `@Metadata` `Function.receiver_type`, `metadata_receiver_types`) is a collection type the actual
  receiver is not a subtype of (`Classpath::kotlin_subtype` over the builtins hierarchy). So
  `MutableCollection.plusAssign` applies to `MutableList`/`ArrayList` but NOT to a read-only `List`, which
  then lows as `list = list.plus(x)`. Names are overloaded across receivers (`plus` on
  `Collection`/`Map`/`Set`), so the receiver set is UNIONed across facade parts and "subtype of any" admits
  the call — first-wins dropped `Iterable.forEach` and broke read-only iteration. No erased type makes the
  decision; the JVM descriptors are only lookup keys.
- For a mutable receiver the (inline) `plusAssign` body is spliced (`MutableCollection.plusAssign` →
  `add`/`addAll`) by the existing bytecode inliner (`Callee::Static{inline:true}`).
- Box gate **1285, 0 FAIL** (+183 vs 1102), gate ~19s. TDD: `feature_box_e2e::CollectionPlusAssign`
  (MutableList/Set/Map + concrete ArrayList mutate; read-only `List += x` reassigns and does NOT mutate the
  original) and `metadata_return_types::{builtins_supertypes_decode_collection_hierarchy,
  kotlin_collection_subtyping, plus_assign_receiver_is_mutable}`.
- Follow-up: the gate is keyed lazily by `@Metadata` only for collection receivers (cheap); generalizing
  Kotlin-receiver applicability to ALL extension resolution (and indexing extensions by their Kotlin
  receiver) would let the same mechanism replace remaining JVM-erased shortcuts. The `arg_fits`/
  `supertype_descriptors` JVM-erased lookup remains as the candidate-enumeration layer.

### Phase 421 — numeric overload resolution prefers the widest int (`until` MIN_VALUE guard)  ✅
- krusty collapses `Byte`/`Short`/`Int` → `Ty::Int` (`desc_to_ty`), so numeric overloads that differ only
  in a `Byte`/`Short` vs `Int` parameter become indistinguishable after parsing — `RangesKt.until(Int,Int)`,
  `until(Int,Byte)`, `until(Int,Short)` all parse as params `[Int,Int]`. The pick landed on the `Byte`
  overload (descriptor `(IB)`), which — unlike the `Int` one — has NO `MIN_VALUE` guard, so a *value-form*
  `2 until Int.MIN_VALUE` wrapped to `2..Int.MAX_VALUE` (a near-infinite range) instead of being empty.
- Fix: in `extension_callable`, `matches.sort_by_key(descriptor_narrowing)` (count of `Byte`/`Short`
  primitive params) before the most-specific pick — preferring the WIDEST descriptor, which is how kotlinc
  resolves an `Int` argument (to the `Int` overload). General: any numeric-overloaded stdlib function now
  selects the `Int` variant for an `Int` arg, matching kotlinc.
- Box gate **1102, 0 FAIL** (the corpus files exercising this also need collection `+=` to compile, deferred
  — see roadmap memory). TDD: `feature_box_e2e::UntilIntOverloadGuard` (`2 until Int.MIN_VALUE` is empty;
  a normal `0 until 5` still iterates 0..4). This is the one independently-valuable piece extracted from the
  (reverted) collection-`+=` work; the full read-only/mutable refactor is the next big phase (memory).

### Phase 420 — emit-erasure infrastructure for Kotlin collection types  ✅
- Prerequisite for keeping `kotlin/collections/{List,MutableList,…}` distinct in the front end: every
  Ty→JVM-name emit point must erase them to the single JVM interface (`java/util/List`), or Kotlin-only
  names would leak into bytecode (`instanceof`/`checkcast`/method-owner refs, descriptors).
- `to_jvm_internal` now erases `kotlin/collections/*` → `java/util/*` (via `kotlin_builtin_to_jvm` on the
  simple name) as a ONE-WAY emit mapping (NOT added to the bidirectional `TYPE_MAP`, so `to_kotlin_internal`
  never has to ambiguously reverse a raw `java/util/List` to `List` vs `MutableList`). `ref_internal` (the
  instanceof/checkcast/method-owner namer) now routes through `to_jvm_internal` instead of using the raw
  `Ty::Obj` name (a latent leak fixed: it also now erases `kotlin/Any` etc.). `Ty::descriptor` already
  routed through `to_jvm_internal`.
- No-op today (nothing produces `kotlin/collections/*` Tys yet) so the box gate holds at **1102, 0 FAIL**;
  this is the safe landing strip for phase 421 (flip `resolve_callable` to the `@Metadata` Kotlin types).
  Unit test `jvm_class_map::tests::collection_types_erase_to_jvm_at_emit`.

### Phase 419 — `@Metadata` function return-type decoding (read-only/mutable foundation)  ✅
- ROOT CAUSE found (with the maintainer): krusty erases `List`/`MutableList` (and `Map`/`MutableMap`, …)
  to `java/util/List` in the FRONT END, so it can't distinguish a read-only collection from a mutable one
  (`roList.add()` wrongly accepted; `coll += x` can't choose `plus`-reassign vs `plusAssign`). The
  distinction is NOT in the JVM descriptor OR the JVM generic `Signature` — both report `java/util/List<T>`
  for `listOf` AND `mutableListOf` (verified via `javap`). It lives ONLY in `@kotlin/Metadata`.
- Foundation built here: `metadata.rs` now decodes each `Package` function's Kotlin RETURN type, faithful
  to kotlinc's reader: (a) `decode_d1` drops the leading `UTF8_MODE_MARKER` (U+0000) per
  `BitEncoding.decodeBytes`; (b) `split_d1` separates the delimited `StringTableTypes` prefix from the
  `Package`; (c) a full `JvmNameResolver` (`StringTableTypes` records expanded by `range` +
  `PREDEFINED_STRINGS` table + `NONE`/`INTERNAL_TO_CLASS_ID`/`DESC_TO_CLASS_ID` ops + substring/replace)
  resolves a `Type.class_name` id (`Function.return_type = 3`, `Type.class_name = 6`) to its Kotlin
  internal name. `package_function_return_types` exposes name -> Kotlin return type.
- VERIFIED vs the real stdlib: `mutableListOf` -> `kotlin/collections/MutableList`, `listOf` ->
  `kotlin/collections/List`, `emptyList` -> `List`, `arrayListOf` -> `java/util/ArrayList`. TDD:
  `tests/metadata_return_types.rs`. The `decode_d1`/`package_inline` rewrite (now splits off the ST prefix
  instead of skip-tolerating it) held the box gate at **1102, 0 FAIL** (inline detection, which feeds the
  bytecode splicer, unchanged).
- NEXT (wires it in): `resolve_callable`/`resolve_type` use the `@Metadata` Kotlin types; the front end
  keeps `kotlin/collections/{List,MutableList,…}` distinct; `to_jvm_internal`/`ref_internal` erase to
  `java/util/*` only at emit; read-only types reject mutators; then collection `+=` is correct (mutable ->
  `plusAssign` inline-spliced, read-only -> `plus`-reassign) with no hardcoding.

### Phase 418 — stepped ranges: `Char` step element type + overflow-safe termination  ✅
- Two coupled bugs in `for (i in a..b step n)` (`Stmt::For`): **(1)** the checker validated the `step` value
  against the *element* type, so a `Char`/`Byte`/`Short` range (`'a'..'e' step 2`) rejected its `Int` step
  with "type mismatch: Int but Char" — but Kotlin's `step` is always `Int` (`Long` for a Long/ULong
  progression). **(2)** the loop broke on `i == end`, which a non-unit step may never hit near
  `MAX_VALUE`/`MIN_VALUE`, so `i ± step` wrapped past the bound and looped forever / produced wrong
  elements (`MaxI-5..MaxI step 3`).
- Fixes: (1) the step's expected type is `Int` (`Long` only for a `Long`/`ULong` range). (2) for a stepped
  signed `Int`/`Long`-family range, break when the NEXT value would pass `end` OR wraps around (`next < i`
  ascending / `next > i` descending detects the overflow) — overflow-safe without a wider accumulator, so
  it covers `Long` too. Matches kotlinc's `getProgressionLastElement` semantics.
- Box gate **1091 → 1102 (+11), 0 FAIL** (unblocks stepped-range corpus files: char ranges, and
  `ranges/literal/inexactToMaxValue`/`inexactDownToMinValue` overflow edges). TDD:
  `feature_box_e2e::SteppedRangeCharAndOverflow`. With phase 417 (Char companion const) this clears 2 of the
  3 pre-existing bugs blocking the (ready, +111) classpath collection `+=` — re-applying that is next.

### Phase 417 — `Char.MAX_VALUE`/`MIN_VALUE` companion constants keep their `Char` type when boxed  ✅
- A `Char` companion constant is read back from the classpath as an integer `ConstantValue`, and lowering
  emitted it as `IrConst::Int` — so in a vararg/generic position (`listOf(Char.MAX_VALUE, …)`) it boxed to
  `Integer`, not `Character` (the list printed `[65535, 0]` instead of `[￿,  ]`). The checker
  already typed `Char.MAX_VALUE` as `Char`; only lowering lost it.
- Fix: when the companion's owner is `Char`, emit `IrConst::Char` (`char::from_u32(v)`), so the constant
  boxes to `Character` and equals the corresponding `Char` literal. `val c: Char = Char.MAX_VALUE` already
  worked (a direct coercion); this fixes the boxed/collection case.
- Box gate **1091, 0 FAIL** (no count change yet — the corpus files needing this also need the classpath
  collection `+=` to compile, see roadmap memory). TDD: `feature_box_e2e::CharCompanionConst`. This is one of
  the three pre-existing bugs that block landing collection `+=` (which is implemented and gives +111).

### Phase 416 — user `plusAssign`/`minusAssign`/… operators (`+=` on a `val`)  ✅
- `target op= rhs` where `op=`'s receiver has a user-defined `plusAssign` (etc.) operator is an IN-PLACE
  CALL (`target.plusAssign(rhs)`), legal even on a `val` — NOT a reassignment. krusty's parser desugars
  `op=` to `target = target op rhs`, so the checker hit its `'val' cannot be reassigned` guard and rejected
  (the single biggest standard-Kotlin skip bucket — 217 first-errors in the front-end survey).
- Fix: the checker (`try_user_plus_assign`, called atop `Stmt::Assign`/`Stmt::AssignMember`) detects a
  desugared compound assign whose target type has a USER `plusAssign`/`minusAssign`/`timesAssign`/
  `divAssign`/`remAssign` (member via `method_of`, or extension via `ext_funs`), type-checks the argument,
  and marks the statement in new `TypeInfo.plus_assign`. The lowerer (`lower_plus_assign`) emits the call:
  member → `invokevirtual recv.opAssign(arg)`, extension → `invokestatic owner.opAssign(recv, arg)`.
- **SCOPED TO USER OPERATORS** (member of a source class / source extension fn): a classpath `+=` such as
  `MutableList += x` (whose `plusAssign` is `@InlineOnly`, no static body to splice) is NOT in `method_of`/
  `ext_funs`, so it keeps its existing `target = target + rhs` lowering — no regression. SOUND because for a
  `val`, `val = val op rhs` can only have come from `val op= rhs` (explicit `val = …` is always an error).
- Box gate **1087 → 1091 (+4), 0 FAIL**. TDD: `feature_box_e2e::UserPlusAssign` (member + extension opAssign
  on a `val` property and a local `val`). Corpus `objects/compoundAssignmentToPropertyWithQualifier` now
  box()=OK (val-property extension plusAssign, object `val`, nested anon).

### Phase 415 — data-class `equals` byte-identical + `instanceof` branch fusion (bytecode parity)  ✅
- kotlinc's data-class `equals` has a specific shape krusty diverged from on three counts: (1) a missing
  `if (this === other) return true` referential-identity fast-path; (2) the `other !is T` guard
  materialized a boolean (`instanceof; iconst_1; ixor; ifeq`) instead of kotlinc's direct
  `instanceof; ifne <ok>` branch; (3) `other` was re-`checkcast` on every field access instead of cast
  ONCE into a local (`checkcast; astore_2`, then `aload_2`).
- Fixes: (A) `emit_cond_branch` now fuses an `InstanceOf`/`NotInstanceOf` (reference target) condition
  into `instanceof; if{ne,eq}` — no 0/1 boolean — the same fusion the comparison ops already had; this is
  general (every `when`/`if` with an `is`/`!is` condition benefits). (B) the `equals` synth emits the
  identity fast-path (new `guard_return_bool`, reusing the existing `RefEq`→`if_acmp` fusion), then the
  `!is` guard, then `val o = other as T` into a local (`IrExpr::Variable`), with each field read off the
  local. Field compares (`Intrinsics.areEqual` for refs, `if_icmp` for primitives) were already correct.
- A `data class D(val s: String, val n: Int)` `equals` is now **byte-identical** to kotlinc 2.4.0
  (verified differentially). The shared `instanceof`-fusion change held the box gate at **1087 OK, 0 FAIL**.
  TDD: `bytecode_parity_e2e::data_class_equals_is_byte_identical_to_kotlinc`.

### Phase 414 — data-class `hashCode`: non-null `String` field via `String.hashCode` (bytecode parity)  ✅
- kotlinc hashes a non-null reference field via `invokevirtual <type>.hashCode()` (so a non-null `String`
  field is `s.hashCode()`); krusty routed ALL references through `Objects.hashCode` (functionally correct,
  byte-divergent). Closed the most common case: a non-null `String` field now hashes via the existing
  `kotlin/Any.hashCode` virtual callee (→ `invokevirtual java/lang/String.hashCode()I`). `field_hash` gains
  a `nullable` flag (from the field's lowered `IrType`, via new `field_nullable`); a `String?` field stays
  on the null-safe `Objects.hashCode`.
- With phase 412 this makes a `data class D(val s: String, val n: Int)` `hashCode` **byte-identical** to
  kotlinc 2.4.0 (verified differentially). Box gate **1087 OK, 0 FAIL**. TDD:
  `bytecode_parity_e2e::data_class_nonnull_string_hashes_via_string_hashcode`.
- Deferred: non-`String` non-null reference fields (user class → `invokevirtual C.hashCode`, but interface
  / type-param / value-class fields must NOT use `invokevirtual` and need the class-vs-interface +
  value-class discrimination); nullable non-`String` refs still use `Objects.hashCode` instead of kotlinc's
  null-guarded ternary. Both stay functionally correct on the `Objects.hashCode` path.

### Phase 413 — data-class `Object`-overrides emitted non-`final` (bytecode parity)  ✅
- kotlinc leaves a data class's `Object`-overrides (`toString`/`hashCode`/`equals`) `public` (open) even
  in a final class — they override open `Object` members — but emits `component`/`copy`/`getX` as
  `public final`. krusty marked EVERY instance method of a final class `final` (the class-final rule),
  so the three overrides diverged.
- Fix: added `IrFile.open_methods: HashSet<FunId>` (methods kotlinc keeps non-`final`); the data-class
  synth inserts the `toString`/`hashCode`/`equals` fids; `emit_method` omits `ACC_FINAL` for a fid in
  that set. SAFE direction — emitting non-`final` is always JVM-legal, and Kotlin forbids overriding a
  non-open member anyway, so nothing regresses. Now byte-matches kotlinc's data-class member flags.
- Box gate **1087 OK, 0 FAIL**. TDD: `bytecode_parity_e2e::data_class_object_overrides_are_not_final`
  (asserts toString/hashCode/equals are NOT final, component/copy ARE).
- (The general method-level `open`/`override` flag model — a user `override fun` of an open base, an
  `open` member in an open class — is still approximated by the class-final rule; only divergent in
  byte flags, never miscompiles. A future phase can generalize `open_methods` to cover it.)

### Phase 412 — data-class `hashCode`: boxed-primitive hashes + `result` local (bytecode parity)  ✅
- kotlinc hashes each primitive field through its boxed static `X.hashCode(prim)` (`Integer.hashCode(I)`,
  `Byte.hashCode(B)`, `Short.hashCode(S)`, `Character.hashCode(C)`, plus the already-handled
  Long/Float/Double/Boolean), and — for **≥2** fields — folds into a `result` LOCAL with an explicit
  `istore`/`iload` round-trip per field (`result = h(f0); result = result*31 + h(fN); return result`).
  An empty data class returns `0`; a single-field one returns `h(f0)` directly (no local). krusty built a
  pure expression tree and passed raw ints for `Int`/`Short`/`Byte`/`Char` — both diverged.
- Fix: `field_hash` routes those four primitives to the boxed `hashCode`; the hashCode synth emits the
  `result`-local shape (`IrExpr::Variable` for the first field, `SetValue` for the rest) for ≥2 fields.
  Added the four `hashCode` descriptors to the emitter's static-helper table.
- **All-primitive** data-class `hashCode` is now **byte-identical** to kotlinc 2.4.0 (verified
  differentially on an 8-field class). Box gate **1087 OK, 0 FAIL**. TDD:
  `bytecode_parity_e2e::data_class_primitive_hashcode_is_byte_identical_to_kotlinc`.
- Deferred (next phases): a **reference** field still hashes via `Objects.hashCode` (functionally correct)
  rather than kotlinc's `field.hashCode()` for a non-null class / null-guarded ternary for a
  nullable-or-type-param field (needs class-vs-interface + nullability discrimination). And the
  data-class Object-overrides (`toString`/`hashCode`/`equals`) are emitted `public final`, but kotlinc
  leaves them `public` (open, as Object-overrides) — `component`/`copy`/`getX` ARE `final` in both.

### Phase 411 — data-class `copy` null-checks non-null reference params (bytecode parity)  ✅
- kotlinc guards each non-null reference `copy` parameter with `Intrinsics.checkNotNullParameter(p, "p")`
  at method entry — the same null-checks the constructor emits — and never a primitive one. krusty's
  synthesized `copy` had empty `param_checks`. Since `copy`'s parameters ARE the primary-ctor properties,
  it takes the SAME guards: in `synth_data_members` we copy the class's precomputed `ctor_param_checks`
  (already correct re: nullability + type-params) onto the `copy` function (resized to the param count).
- Verified byte-identical to kotlinc 2.4.0 (`javap -c`: `copy` of `data class D(val s: String, val n: Int)`
  emits one `checkNotNullParameter` for `s`, none for `n`, then `new D`). Box gate **1087 OK, 0 FAIL**.
  TDD: `bytecode_parity_e2e::data_class_copy_null_checks_nonnull_reference_params`.
- Remaining data-class parity gaps (each a future phase): synth methods are `public` not `public final`
  (only matters in open/abstract classes — final classes already correct); `hashCode` boxes an `Int`
  field to `Objects.hashCode(Object)` + a temp local instead of `Integer.hashCode(I)` on the stack.

### Phase 410 — data-class member emission order (bytecode parity)  ✅
- kotlinc emits data-class members as `componentN, copy, copy$default, toString, hashCode, equals`;
  krusty appended `copy`/`copy$default` LAST (after toString/hashCode/equals). Moved the `copy` synth
  block before `toString` in `synth_data_members` so the order matches. Runtime-identical → box gate 1086
  OK, 0 FAIL. TDD: `bytecode_parity_e2e::data_class_member_order_matches_kotlin` (asserts
  componentN < copy < toString).
- Remaining data-class parity gaps (each a future phase): synth methods are `public` not `public final`;
  `copy` lacks `checkNotNullParameter` on non-null reference params; `hashCode` boxes an `Int` field to
  `Objects.hashCode(Object)` + a temp local instead of `Integer.hashCode(I)` on the stack.

### Phase 409 — data-class `toString` → single `StringBuilder` (bytecode parity)  ✅
- The synthesized `data class` `toString` chained `String.plus` (one `StringBuilder` per `+`); kotlinc
  emits ONE. Rebuilt it as a single `IrExpr::StringConcat` (the phase-401 node): the class name + first
  field name merge into one `"P(x="` constant, then field values, `", name="` separators, and `")"` (a
  single char → `append(C)`). Verified vs kotlinc: `P.toString` now ONE StringBuilder, `ldc "P(x="`,
  `append(I)`, `append(", y=")`, `append(C)`. Removed the now-unused `str_plus` helper. Box gate 1086 OK,
  0 FAIL (runtime-identical). TDD: `bytecode_parity_e2e::data_class_tostring_uses_single_stringbuilder`.
- (A separate data-class parity gap remains: member *emission order* — krusty emits `copy`/`copy$default`
  in a different order than kotlinc; a future ordering pass.)

### Phase 408 — multifile: cross-file class method calls + property writes  ✅
- Completes cross-file class *use*: an instance method call (`b.m(args)`) and a `var` property write
  (`b.tag = v`) on a class declared in ANOTHER file now lower to `CrossFileVirtual` (`invokevirtual`
  the method / `setX(v)`), not a bail. `ir_lower`: the member-call arm gets a sibling-file branch (own
  methods, exact arity; inherited/vararg/defaulted bail) after the local user-method branch; the
  `AssignMember` arm gets a sibling-file `var`→`setX` branch before its `class_of(rt)?`. Value-class
  receivers still bail. **Box conformance: 1085 → 1086 box()=OK, 0 FAIL.**
- TDD: `cli_dropin_e2e::cross_file_class_construct_and_property_read` extended — construct + property
  read + method call + `var` write across files, run to "OK". Cross-file class *use* (construct, field
  read/write, method call) is now functional; remaining cross-file gaps: inherited members, enums/objects.

### Phase 407 — multifile: cross-file class construction + property read  ✅
- Constructing a class declared in ANOTHER file and reading its property now lower to cross-file
  bytecode (no bail). New backend-agnostic IR: `IrExpr::NewCrossFile { internal, params, args }` (→ `new
  internal; dup; <args>; invokespecial internal.<init>(desc)`, descriptor built in the JVM emitter) and
  `Callee::CrossFileVirtual { owner, name, params, ret, interface }` (→ `invokevirtual`/`invokeinterface`).
  `ir_lower`: `lower_external_new` routes a sibling-file user class (found by internal name in
  `syms.class_by_internal`, not in this file's IR classes) to `NewCrossFile`; the member-read arm routes a
  sibling-file property to its `getX()` via `CrossFileVirtual`. No driver map needed — the class is
  referenced by its own internal name. **Bails (skip, never miscompile):** a sibling-file value class
  (unboxed, no instance `<init>`), annotation, or inner class.
- **Box conformance: 1084 → 1085 box()=OK, 0 FAIL** (value-class cross-file shapes correctly skip).
- **Drop-in finding:** unblocking cross-file `Point()` made `compiles_directory_to_jar_consumable_by_kotlinc`
  reach the kotlinc-consumer step (it skipped at compile before) — kotlinc can't `import demo.mk` because
  krusty's facade `@Metadata` doesn't fully describe top-level functions. krusty emits a minimal
  `@Metadata` (jar is JVM-runnable) but full kotlinc-source consumption needs complete `@Metadata` (a
  protobuf blob) — a known gap; the test now skips that step with a note.
- NEXT cross-file-class steps: instance method calls (`b.m()` → `CrossFileVirtual`) and property writes.

### Phase 406 — multifile: cross-file top-level property access  ✅
- A read/write of a top-level property declared in ANOTHER file now lowers to the other facade's
  accessor (`invokestatic <facade>.getX()` / `setX(v)` — the field is private since phase 398), instead
  of bailing. Added `SymbolTable.prop_facades` (prop name → `(facade, type, is_var)`, driver/harness-
  populated like `fn_facades`), reusing the backend-agnostic `Callee::CrossFile` for the accessor call.
  ir_lower: a `Name` read missing local statics but in `prop_facades` → `getX` call; `Stmt::Assign` to a
  cross-file `var` → `setX` call (a cross-file `val` write bails). Driver + `compile_multifile` populate
  the map. **Box conformance: 1079 → 1084 box()=OK, 0 FAIL.**
- TDD: `cli_dropin_e2e::cross_file_function_and_property` (function + property read + var write across
  files, run to "OK"). Single-file path unchanged.

### Phase 405 — multifile: conformance harness splits `// FILE:` blocks  ✅
- The conformance harness now compiles a `// FILE: name.kt`-split test as ONE module (`compile_multifile`):
  split on the markers, parse each block, collect GLOBAL signatures over all files, populate
  `SymbolTable.fn_facades` (cross-file fn→facade, like the CLI driver), then check + lower + emit each
  file and run `box()` against ALL emitted classes. `// MODULE:` (separate classpaths) stays skipped; a
  file using an unmodeled cross-file construct (e.g. a cross-file *class* reference) makes lowering bail →
  the test SKIPS (never miscompiles). This converts phase 404's cross-file-function codegen into real
  corpus coverage. **Box conformance: 1076 → 1079 box()=OK, 0 FAIL** (the first multifile tests pass).
- Modest today (only cross-file-*function*-only multifile tests pass); rises as cross-file classes /
  properties land. Single-file path unchanged.

### Phase 404 — multifile: cross-file top-level function calls  ✅
- A call to a top-level function defined in ANOTHER source file of the same compilation now lowers to a
  cross-facade `invokestatic` instead of bailing. The driver already runs global signatures + per-file
  lowering; the missing piece was codegen knowing the *other* file's facade. Added (no signature
  threading): `SymbolTable.fn_facades` (fn name → facade internal), populated ONLY by the multi-file
  driver (it knows each file's stem→facade); a backend-agnostic `Callee::CrossFile { facade, name,
  params, ret }` (carries `IrType`s so `ir_lower` builds no JVM descriptor — the JVM emitter does);
  `ir_lower` emits it for a `Name` call that misses local `fun_ids` but hits `fn_facades` (simple
  exact-arity case; vararg/defaults bail); JVM `emit` → `invokestatic <facade>.<name>(desc)`; JS by name.
- Single-file/in-process callers leave `fn_facades` empty → unchanged (box gate 1076 OK, 0 FAIL).
- **TDD:** `cli_dropin_e2e::cross_file_top_level_function_call` — compiles `A.kt` (helper/tag) + `B.kt`
  (box calling them) with the krusty binary, links via `javac`, runs `box()` → "OK".
- NEXT multifile steps (each a phase): cross-file top-level *property* access (via the other facade's
  `getX`/`setX`), then the conformance harness splitting `// FILE:` blocks to actually exercise the 1330
  multifile corpus tests (this codegen is +0 corpus until the harness does that).

### Phase 403 — safe-call + elvis primitive fusion (no boxing)  ✅
- `recv?.<prop> ?: default` with a PRIMITIVE result no longer boxes. krusty lowered `s?.length` to a
  boxed `Integer?` (the safe-call must be null-capable) and the elvis then unboxed it — `Integer.valueOf`
  + `checkcast` + `intValue`. kotlinc instead null-checks the receiver and selects the unboxed member or
  the default (`ifnull`/primitive path). New `Lower::lower_safe_prop_member` builds `(var, cond, member)`
  for a no-arg safe property/length access (unboxed member); the `Elvis` arm uses it when the result is
  primitive, emitting `when { recv != null -> member; else -> default }` with no boxing. Verified
  `s?.length ?: -1` → `ifnull` + `String.length()`, no `Integer.valueOf`. Box gate 1076 OK, 0 FAIL.
- **TDD:** `bytecode_parity_e2e::safe_call_elvis_primitive_does_not_box` (asserts no `Integer.valueOf`,
  presence of fused `ifnull` + `String.length`) + runtime cases in the same test.

### Phase 402 — `for (i in (a..b).reversed())` over a literal range  ✅
- Iterating a `.reversed()` *literal* `..`/`downTo` range — `for (i in (1..4).reversed())` — is rewritten
  in the parser to the reversed counted `ForRange` (`4 downTo 1`), so the checker/lowering see a normal
  `downTo` loop (no new IR, no value-class/range-iterator machinery). Only side-effect-free bounds (a
  literal or a name) are rewritten: kotlinc evaluates a reversed range's bounds in SOURCE order, so a
  call-bound `(logged()..logged()).reversed()` keeps the iterable path (skips) — guarded after the
  `forInRangeLiteralReversed` evaluation-order test showed the swap. Both `(a..b)` (a `RangeTo`) and the
  value-form `(a downTo b)` (which parses as the infix call `a.downTo(b)`) are handled → `b downTo a` /
  `b..a`. `until`-reversed (`(a until b).reversed()` → `(b-1) downTo a`) is also handled: the `hi-1` is
  built after the simplicity check (which is on the ORIGINAL bound). All `..`/`downTo`/`until` reversed
  literal forms now lower. TDD: feature snippet `ForInReversedLiteralRange` (`..`, `0..3`, `downTo`,
  `until`). Box gate 1076 OK, 0 FAIL (a capability step; corpus `forInReversed` files carry other
  blockers, so +0 today, but the `.reversed()` blocker is now gone for them).

### Phase 401 — string templates → single `StringBuilder` (bytecode parity)  ✅
- krusty lowered a template `"a${x}b"` to a chain of `String.plus` calls — the backend emitted ONE
  `StringBuilder` per `+` (4 nested StringBuilders for a 5-part template). New `IrExpr::StringConcat(parts)`:
  the lowerer drops empty literal chunks and emits one node; the backend emits kotlinc's shape — a single
  interpolation `"$x"` → `String.valueOf(x)` (typed overload); multiple parts → ONE `StringBuilder` with a
  typed `append` per part (single-char string literal → `append(C)` with the char constant) + `toString`.
- **Value-class encapsulation kept:** `ir_lower` has no value knowledge; `value_classes` boxes a value-class
  `StringConcat` part (so `append(Object)`/`valueOf(Object)` calls the value class's `toString`), exactly as
  it did for `String.plus` args — `collect_reachable` + the box-at-boundary set both learned `StringConcat`.
  Verified byte-exact vs kotlinc on `"x=$a y=$b!"` (one SB, `append(C)` for `"!"`). Box gate 1076 OK, 0 FAIL.
- **TDD:** new `tests/bytecode_parity_e2e.rs` — 8 tests asserting the exact codegen of phases 397–401
  (`iinc`, compare-to-zero, `dcmpl`, fused `if_icmp`, single-StringBuilder + `append(C)` + `valueOf`,
  top-level property ABI) PLUS a differential check that a counting loop is byte-identical to real kotlinc.

### Phase 400 — `iinc` + compare-to-zero (bytecode parity)  ✅
- Two pervasive loop/branch codegen fixes found via `bytediff`:
  - **`iinc`**: `i = i + k` / `i = k + i` / `i = i - k` on an `Int` local with a small constant `k` now
    compiles to `iinc slot, k` (kotlinc's form) instead of `iload;iconst;iadd;istore`. Every counting loop.
  - **compare-to-zero**: a comparison with the integer literal `0` (`x != 0`, `x < 0`, …) uses the
    single-operand `ifeq`/`iflt`/… branch (kotlinc's form) instead of `iconst_0;if_icmp*`. `0 <op> x` is
    normalized via `swap_cmp`. Ubiquitous (loop bounds, guards).
- Together these make a whole class of loops byte-identical: e.g. `forEachIntArray.kt` now matches
  kotlinc's `box()` instruction-for-instruction (verified by normalized `javap` diff). Box gate 1076 OK,
  0 FAIL. Aggregate `bytediff` on the 60-file sample: **30.3% → 32.6%** byte-identical (and the broader
  loop/comparison shape now matches kotlinc everywhere these patterns occur, even where other divergences
  keep a class from being fully identical).

### Drop-in finding — Kotlin `@Metadata` not emitted (Kotlin↔Kotlin interop gap)
- Phase 398 made top-level properties **Java-consumable** (a real interop milestone — verified: `javac`
  compiles + links against krusty's `getX`/`setX`). But a *Kotlin* consumer (real kotlinc) importing a
  krusty-compiled declaration FAILS: kotlinc resolves Kotlin declarations from the `@Metadata` annotation
  (a protobuf blob), which krusty does not emit. So krusty output is consumable by Java but NOT by kotlinc.
  This is a major standalone feature required for full drop-in (every public declaration needs `@Metadata`).
  Tracked; `top_level_property_e2e` part 2 skips on it (part 1 — the Java ABI — is asserted).

### Phase 399 — float/double compare `dcmpl`/`fcmpl` for `>`/`>=` (bytecode parity + NaN)  ✅
- krusty used `dcmpg`/`fcmpg` for ALL float/double comparisons; kotlinc uses the `*l` variant for `>`
  and `>=` (NaN → -1) and the `*g` variant for `<`/`<=` (NaN → +1), so a NaN operand makes the
  comparison false either way. Added `dcmpl`/`fcmpl` to `CodeBuilder`; both `emit_compare` and the fused
  `emit_compare_branch` now pick `*l` for `Gt`/`Ge`. Verified `a > b` → `dcmpl;ifle` (kotlinc's exact
  shape). Also a NaN-comparison *correctness* fix. Box gate 1076 OK, 0 FAIL.

### Phase 398 — top-level property field modifiers + accessors (bytecode parity)  ✅
- Closed parity divergence #2. krusty emitted a top-level `val`/`var` as a bare `public static` field
  with no accessor; kotlinc emits `private static final` (val) / `private static` (var) **plus** a
  `public static final getX()` (and `setX()` for a `var`, with `checkNotNullParameter("<set-?>")` on a
  non-null reference param). `const val` stays `public static final` with no accessor (kotlinc inlines it).
- `IrStatic` gains `is_var`/`is_const`. `emit_statics` emits the kotlinc field flags + accessors; a
  `GetStatic`/`SetStatic` reads/writes the private field DIRECTLY from within the facade but routes
  through `getX()`/`setX()` from any other class (kotlinc's cross-file property-access compilation).
- Verified byte-exact vs kotlinc on `val x; var y` reference (`private static final int x` + `getX` +
  `getY` + `setY`). Box gate held 1076 OK, 0 FAIL; property e2e green. (Parity % on the annotation/array-
  heavy 30-file prefix is flat — those files have no top-level vals; the fix is exact where it applies.)

### Phase 397 — comparison→branch fusion (bytecode parity)  ✅
- Closed parity divergence #1 (the biggest lever). krusty *materialized* a 0/1 boolean for every
  comparison and tested it with `ifeq`/`ifne` (`iload;iload;if_icmplt L;iconst_0;goto;iconst_1;ifeq`);
  kotlinc fuses the comparison into the branch. New `emit_cond_branch`/`emit_compare_branch` in
  `ir_emit` emit a single inverted-polarity jump (`if_icmpge`/`ifnull`/`if_acmpeq`/`lcmp;ifge`/
  `areEqual;ifeq`) instead. Wired into every conditional-branch site: `While` pre-test, `do…while`
  post-test, and each `when`/`if` branch condition. Runtime-identical → box gate stays 0 FAIL.
- **Parity: ~9.5% → ~13.6%** normalized-byte-identical (measured by `bytediff`, samples differ in size
  but the loop/if `if_icmp*` shape now matches kotlinc exactly — verified on `for (i in 0 until 4)`).
- Remaining parity backlog: top-level `val`/`var` field modifiers + getter routing; annotation
  instances as interfaces; float compare `dcmpg`/`dcmpl` NaN-polarity selection (krusty always `dcmpg`).

### Phase 396 — bytecode-parity instrument + baseline  ✅
- `src/bin/bytediff.rs`: normalized `javap -c -p` diff of krusty vs real kotlinc per class (strips
  source banner, bytecode offsets, constant-pool indices; keeps signatures + instruction mnemonics +
  operands + resolved `// …` comments). The first measurement of the project's *bytecode-equality* goal
  (the `box()=OK` gate only proved runtime correctness). Opt-in, slow (one kotlinc launch/file), not in
  the <60s gate. Docs in `docs/DIFF_KOTLINC.md`.
- **Baseline (first 15 both-compile files):** ~9.5% classes normalized-byte-identical. RANKED divergences
  (the bytecode-parity backlog):
  1. **Loop shape (biggest lever — every loop):** krusty emits test-at-bottom (`goto TEST; BODY; TEST:
     if_icmplt BODY`), kotlinc emits test-at-top exit-forward (`if_icmpge END` at the top). Affects all
     `forEach*Array`/range/while loops. Runtime-equivalent, so the box gate stays green — pure parity.
  2. **Top-level `val`/`var` field:** krusty emits a `public static` field; kotlinc emits `private static
     final` (val) / `private static` (var) + a `public static getX()`/`setX()` and routes cross-class
     reads through the getter. Needs getter/setter emission + read-via-getter from other classes.
  3. **Annotation instances:** krusty emits `final class A`; kotlinc emits `interface A extends
     java.lang.annotation.Annotation` + a synthetic `<facade>$annotationImpl$A$0` impl. Structural.
  4. **Branch-condition polarity** (`if_icmpeq`/`if_icmplt` vs kotlinc's inverted `if_icmpne`/`if_icmpge`)
     — falls out of the loop-shape fix.
  Method: pick a divergence → fix the emitter → re-run `bytediff` → confirm the % rises with box gate at
  0 FAIL. NEXT parity phase: match kotlinc's loop codegen shape (item 1).

### Phase 395 — classes with no primary constructor  ✅
- Support `class A { constructor(…) { … } }` (no primary ctor): each secondary becomes its own `<init>`.
  A `super(…)`/implicit-delegating ctor runs the field initializers + `init {}` blocks before its body;
  a `this(…)`-delegating ctor runs only its body (init runs in the reached super-ctor). Sibling `this(…)`
  and same-name constructor overloads are resolved by argument type. The parenless base class
  (`class A : B { constructor(): super() }`) is recovered in a post-parse fixup (the parser can't tell a
  parenless class supertype from an interface).
- **Field-initializer default-value elision** (kotlinc semantics): a body-property initializer that
  stores the field's JVM default (`0`/`false`/`null`/`'\0'`, incl. `0.toByte()`) is dropped, so a value a
  base constructor's virtual call already wrote survives. SPEC §updated; test `secondary_ctor_noprimary_e2e`.
- Bails (skip, never miscompile): a secondary with a defaulted parameter, an ambiguous `this(…)` target.
  Touched parser/resolve/ir_lower/ir_emit + `IrSecondaryCtor.delegate` (`CtorDelegateTarget::{This,Super}`)
  and `has_primary_ctor` on `ClassDecl`/`IrClass`.
- **Architecture invariant kept:** `ir_lower` has NO knowledge of the JVM value-class transformation —
  it lowers a no-primary class as a plain class. The delegation `<init>` *target signature* is read LIVE
  from the (post-`value_classes`-pass) base/own class in `ir_emit`, so value-class erasure of a base ctor
  is reflected automatically (the value-class `super(…)` cases now compile correctly instead of bailing).
- `src/bin/survey.rs` upgraded to run the FULL pipeline against the real classpath (stdlib + JDK
  `lib/modules`) so skip-reason histograms match the conformance harness (was front-end-only, no stdlib).
- Box conformance after this phase: **7351 scanned · 1076 box()=OK · 0 FAIL** (was 1059).

## Phase 439 — inline branchless-lambda loop hosts (map/forEach/fold/mapIndexed)  ✅
- The unified bytecode splicer now inlines loop-shaped library HOFs (`map`/`forEach`/`fold`/
  `mapIndexed`/…) whose lambda body is **branchless** — previously these fell back to a real
  `invokestatic CollectionsKt.*` call. `extension_callable` marks non-public callees `must_inline`
  and `try_route_lambda_inline` honors it, so a loop host with a straight-line lambda splices its
  body into the iterator loop (verified via `javap`: no `invokestatic CollectionsKt.map`).
- **Frame fix (root cause of the `mapIndexed` "Inconsistent stackmap frames" `VerifyError`):** a
  loop host pushes the destination/accumulator *and the lambda* onto the operand stack, so a host
  frame at a branch target *between* the lambda `aload` and its `invoke` (e.g. `mapIndexed`'s
  index-overflow `ifge`) lists the lambda's `FunctionN` on its frame stack. Splicing deletes that
  `aload`, so the relocated frame must drop the now-dead `FunctionN` operand-stack entry. Done in
  `splice_unified`'s host-frame stack relocation. Test `feature_box_e2e` (`MapIndexed`, `LoopInline`).
- **Remaining frontier (next phase):** a loop host with a *branchy* lambda body still bails to a
  real call — the spliced lambda's own frames need the host operand-stack prefix prepended (the
  dest/acc below the lambda result), not yet threaded. Precisely guarded (`host_has_loop &&
  any_branchy_lambda`): public host → real call, never a miscompile.
- Box conformance after this phase: **7351 scanned · 1313 box()=OK · 0 FAIL**.

## Phase 440 — inline branchy-lambda loop hosts (operand-state simulation; the last inline bail)  ✅
- Removes the last inliner bail: a loop host (`map`/`filter`/…) whose lambda body is BRANCHY now
  splices too. The lambda body's own StackMapTable frames are compiled against an empty operand base,
  but a loop host runs the lambda at a NON-EMPTY baseline — `map`/`filter` keep the destination
  collection on the stack BELOW the lambda result, and the iterated element is stored to a host local
  *after* the loop-head frame. So each lambda-body frame must be rebased onto the host's live state.
- **`host_state_at` (a typed forward operand-stack simulator)** computes that state — the slot-indexed
  locals AND the operand-stack prefix — just before each lambda's `aload`, seeded from the nearest
  host frame and walked straight-line to the load. It models the standard opcodes; any UNMODELED
  opcode, or an opaque `Top` surviving onto the operand prefix, returns `None` → the splice bails for
  that (branchy) lambda and the host falls back to a real call. Never a miscompile.
- `BranchySplice` gains `lambda_stack_prefix` (prepended to each lambda-body frame's stack) and
  `lambda_host_locals` is now sourced from the simulated locals (the prior nearest-frame value was
  stale — it lacked locals assigned later in the loop body, e.g. the element). `VType` is now `Copy`.
- Tests: `feature_box_e2e` `MapBranchy` (map/filter/forEach/fold each with a branchy lambda body);
  unit tests `host_state_at_computes_loop_prefix_and_locals`, `host_state_at_bails_on_surviving_opaque`,
  `method_desc_effect_counts_args_and_return`, `collapse_slots_is_inverse_of_expand`.
- Box conformance after this phase: **7351 scanned · 1313 box()=OK · 0 FAIL** (same count — these
  cases were already correct via fallback; they now INLINE instead of calling the stdlib HOF).

## Phase 441 — exhaustive operand-state opcode coverage (close the simulator's unmodeled-opcode gap)  ✅
- `host_state_at` now models EVERY operand-stack opcode that can legally appear on a straight-line region
  in a v52+ method body: the full `dup` family (`dup_x2`/`dup2`/`dup2_x1`/`dup2_x2` via a category-aware
  `pop_group` helper), `wide` (2-byte-index load/store + `wide iinc`), `monitorenter`/`monitorexit`, and
  `multianewarray`. The previous "unmodeled opcode → fall back" gap (a soft inline bail) is closed.
- The residual `None` returns are now SOUNDNESS BOUNDARIES, not feature gaps: (a) `invokedynamic` can't be
  relocated without bootstrap-method handling — the splice bails on it at `relocate_insns` regardless;
  (b) `athrow`/returns/`jsr`/`ret` are terminal or forbidden in v52+, so they can't precede the lambda
  load on a fall-through path; (c) an opaque value (e.g. an array element) surviving onto the operand
  prefix has no expressible frame type. None of these is a modelable case we decline.
- Tests: unit `host_state_at_models_dup_family` (spec `dup2_x1` form-1 reordering). Gate unchanged at
  **1313 box()=OK · 0 FAIL**.

## Phase 442 — inline frame-recording HOFs at a nonempty caller operand baseline (last inline bail)  ✅
- Closes the final inline fallback: a frame-recording inline call (a loop HOF like `map`/`filter`, a
  branchy lambda body, or a branchy `@InlineOnly` `require`/`check`) used as a NON-FIRST operand — e.g.
  `sb.append(xs.map { if (..) a else b })`, where a dispatch receiver / earlier argument is already on
  the operand stack — previously hit `try_inline_unified`'s empty-baseline guard and fell back to a real
  `invokestatic CollectionsKt.*`. The relocated frames are bound relative to an empty operand base (no
  caller operand prefix is threaded in), so a nonempty baseline can't bind them directly.
- **Fix uses the EXISTING spill mechanism, no CodeBuilder operand-type tracking.** `emit_operands` already
  spills earlier operands to temps (then reloads) when a later operand `records_frame` — keeping the
  splice at an empty baseline (the same path `when`/`try` use). The gap was that `records_frame` didn't
  recognise an inline call whose SPLICE records frames. Now its `IrExpr::Call` arm reports a `Callee::
  Static{inline|must_inline}` whose lambda arg has a branchy `inline_body`, OR whose host body
  disassembles to any branch/switch (loop HOFs, `require`/`check`). `needs_frames` in `try_inline_unified`
  now also counts host frames (`!probe.frames.is_empty()`) so a non-spilled loop host at a nonempty
  baseline bails SAFELY instead of binding prefix-less frames (closes a latent miscompile too).
- Verified via `javap`: `makePair("k", xs.map { branchy })` and `sb.append(xs.filter { … }.toString())`
  now emit iterator loops with NO `invokestatic CollectionsKt.map/filter`. TDD e2e
  `InlineHofNonEmptyBaseline`. Gate **1313 box()=OK · 0 FAIL**, bytecode-parity 16/0 — no regression.
- Inline support is now bail-free for every shape the splicer can represent; the only remaining `None`
  returns are hard soundness boundaries (unrelocatable `invokedynamic`, untypeable operand-prefix slot).

## Phase 443 — relocate exception handlers (inline `synchronized`/`use`/`runCatching`)  ✅
- Closes the `has_handlers` splice bail (3 reachable corpus cases, confirmed by instrumented census).
  `MethodCode` now carries the full exception table (`Vec<ExcEntry>` — `start_pc`/`end_pc`/`handler_pc`/
  `catch_type`) instead of a `has_handlers: bool`. `splice_unified` relocates each entry: byte offsets
  are mapped through `old_off` → `old2new` (+ prologue) → `offs` to absolute spliced offsets, and
  `catch_type` is re-interned into `cw` (0 = catch-all/`finally`). The handler *frames* need no extra
  work — a handler is a StackMapTable target, so it's already relocated by the host-frame pass.
- `BranchySplice.handlers` carries the relocated `(start, end, handler, catch_type)`; `try_inline_unified`
  binds them into the caller's exception table via `bind_at` labels + `add_exception`.
- **Verified via javap:** `synchronized(lock) { … }` now inlines `monitorenter`/`monitorexit` with the
  relocated exception table (`14-25 → 40 any`, `70-112 → 128 any`) — no fallback `invokestatic`. TDD e2e
  `InlineWithHandlers` (two `synchronized` blocks incl. a loop body). Gate **1313/0**, parity 16/0.
- **INDY boundary proven VACUOUS** (the user asked to verify against kotlinc): kotlinc compiles lambdas
  inside `inline` functions as anonymous-class singletons (`getstatic …$N.INSTANCE`), NEVER
  `invokedynamic` — precisely so the inliner can copy them cross-module. Confirmed by compiling a probe
  (`inline fun pick() = consume { 42 }` → `getstatic …$pick$1.INSTANCE`, 0 invokedynamic in the class)
  and by an instrumented census: 0 INDY / 0 host-state bails across all 7351 corpus files.

## Phase 444 — reified type parameters in inline functions (`is T` / `as T`)  ✅
- The IR inliner (`lower_inline_fn_call`) previously bailed any `<reified T>` inline fn (file skipped).
  Now it binds each reified type parameter to the call's explicit type argument (`call_type_args`,
  resolved through any enclosing reified binding so nested reified inlines compose) and substitutes the
  bound type into reified type operations in the expanded body. New `reified_subst` stack on `Lower` +
  `subst_type_ref(&TypeRef)`; applied in the `Expr::Is` and `Expr::As` arms (so `x is T` → `instanceof
  ActualType`, `x as T` → `checkcast ActualType`). A reified arg that isn't an explicit type argument
  (purely inferred) still bails — never a miscompile.
- TDD e2e `ReifiedInline`: `isT<String>`/`isT<Int>`/`isT<Number>`, `asT<String>`, and a reified type
  used inside a nested inline-HOF lambda (`xs.count { it is T }`). Gate **1313/0** — no regression, no
  miscompile (removing the bail exposed no unhandled reified op in the corpus).
- NEXT for full reified: `T::class` (parses as `CallableRef{name:"class"}`), `arrayOfNulls<T>` /
  `Array<T>(n){}`, `enumValues<T>()` — substitute the reified element/class type at those sites too.

## Phase 445 — reified type params in ALL type positions (centralize subst in `ty_ref`)  ✅
- Moves reified substitution into `ty_ref` (the central `TypeRef`→`Ty` resolver), so a reified `T`
  resolves to the bound concrete type in EVERY type position inside an expanded `<reified T>` inline
  body — `Array<T>`, `val x: T`, a `T` return type, a type argument — not just `is`/`as` (the `Is`/`As`
  arms keep their own subst for nullable handling; double-subst is idempotent). Recurses at most once
  (the bound type is already concrete). `reified_subst` is empty outside inline bodies, so `ty_ref` is
  unchanged there. Gate **1313/0** — no regression.
- Remaining reified work (each a distinct feature, not a splice bail): `T::class`/`enumValues<T>` need a
  `KClass`/reflection subsystem (unsupported even for a concrete `Foo::class`); reified array CREATION
  (`arrayOfNulls<T>`/`Array<T>(n){}`) needs the element type threaded through the array-builder path;
  and reified EXTENSION/member inline fns share the general `receiver.is_some()` inline limitation.

## Phase 446 — reified array creation `Array<T>(n){…}` + trailing-lambda type-arg parser fix  ✅
- **Parser fix:** the trailing-lambda postfix branch rebuilt the call expr but dropped its explicit type
  arguments — both `f<T>(args){…}` (consumed into the pre-rebuild call's `call_type_args`) and `f<T>{…}`
  (unconsumed `pending_targs`). Now carries both onto the rebuilt call. This was a long-standing bug
  (noted for `assertFailsWith<T>{}` etc.); fixing it makes `<T>` survive on any call with a trailing
  lambda.
- **Reified array element:** `synth_array_elem` now prefers the call's explicit type argument resolved
  through `ty_ref` (reified-aware), so `Array<T>(n){…}` inside a `<reified T>` inline body allocates a
  real `new String[]` (`anewarray java/lang/String`) rather than the erased `Object[]`. javap-verified.
- TDD e2e `ReifiedInline` extended with `pair<String>(...)` → `Array<String>`. Gate **1313/0** — the
  parser fix touches every trailing-lambda call with no regression.

## Phase 447 — non-local-return inline (inline fn bodies with `return`)  ✅ (+1 → 1314)
- The IR inliner bailed any inline fn whose body had a `return` (`body_has_return`). Now it expands them:
  the body is wrapped in `while(true){ <body>; [result = fall-through;] break@end }` and each `return x`
  lowers to `result = x; break@end` (a new `inline_return` stack of `(slot, label, ret_ty)` consulted in
  `Stmt::Return`). The function return becomes a jump to the body's end — including a `return` out of a
  `for`/`while` loop in the body. Lambda args carrying a return are still pre-bailed, so a surviving
  `return` always belongs to the innermost inline body (sound).
- Frame correctness, three fixes: (a) the result slot is initialized to a type default (an uninitialized
  slot is `top` at the loop head but the body assigns it → mismatch); (b) when the body ALWAYS diverges
  (type `Nothing`), the fall-through assign+break are omitted (they'd be unframed dead code after a
  `goto`); (c) `value_ty(Block)` recovers a block-local slot's type from its `Variable` declaration when
  the slot isn't yet emit-registered (a comparison querying the inline result before the block emits would
  otherwise see `Ty::Error` and pick the reference path → "Bad type on operand stack").
- `try { … } finally { … }` around a `return` in an inlined body is not yet combined with the jump — that
  case bails (file skips, never a miscompile). TDD e2e `InlineNonLocalReturn` (sequential/conditional
  returns, a `return` out of a `for` loop, a `Unit` early `return`). Gate **1314/0** (+1), parity 16/0.

## Phase 448 — user `inline fun` EXTENSIONS (concrete receiver)  ✅
- `lower_inline_fn_call` gains a `recv` parameter: an extension call `recv.foo(args)` resolved to a user
  `inline fun <Recv>.foo()` evaluates the receiver once into a temp and binds it as `this` in the inlined
  body's scope (so `this`/`this.member`/implicit-receiver access resolve), then expands the body —
  composing with reified, non-local return, and lambda-arg inlining. Decl matched receiver-aware; the
  signature comes from `ext_funs`. Wired in before the non-inline `ext_fun_ids` static-call path.
- **General `value_ty` fix:** a `GetValue` of a slot whose `Variable` isn't emit-registered yet (an inline
  result/`this` temp queried by a comparison before its block emits) returned `Ty::Error` → wrong
  (reference) operator path → "Bad type on operand stack". New `var_types` map (every `Variable` index →
  JVM type, file-wide) is a `value_ty(GetValue)` fallback; supersedes the narrow phase-447 Block fix.
- TDD e2e `InlineExtension` (`Int.doubled`/`String.shout`/`Int.clampPos` incl. a non-local-return
  extension). Gate **1314/0**, parity 16/0. Remaining: GENERIC-receiver extension inline (`<T> T.echo()`
  — not in `ext_funs`; needs receiver-type specialization), `T::class`/reflection, `arrayOfNulls`,
  `String[i]` (all pre-existing, orthogonal to inline).

---

## Phase 449 — generic-receiver `inline fun` extensions (`<T> T.foo()`)  ✅
- A user `inline fun <T> T.foo()` now inlines. The receiver type param erases to `kotlin/Any`, so the
  extension is keyed in `ext_funs` under the `Any` descriptor; the checker's method-call resolution now
  falls back to that key for ANY receiver (when no exact match), specializing the return type — a return
  naming the receiver type param → the actual receiver type, one naming a value-param type param → that
  argument's type. Restricted to `inline` decls (a non-inline generic extension needs erased-`Object`
  boxing at the real call, which this path doesn't model — left unresolved/skip, no regression).
- The lowerer's `lower_inline_fn_call` specializes the generic receiver to the actual type (`recv_ty` via
  `self.recv_ty`), derives the value-param/return signature from the decl when `ext_funs` lacks an entry,
  and binds `this`.
- **Cross-cutting compiler fix (`ir_lower` Name lowering):** a smart-cast "narrowing" to `kotlin/Any` is a
  no-op WIDENING to the top type, never a real narrowing — it arose when an inline expansion specialized a
  slot to a more concrete type than the checker's erased `info.ty` (a generic inline param/`this`), and
  the spurious `checkcast Object` it emitted erased the value (`VerifyError: Bad return type`). Now
  skipped. (Found+fixed via `boxing14.kt` triage: also confirmed the checker fallback must be `inline`-only.)
- TDD e2e `InlineExtension` extended with `<T> T.echo()` on `String` and `Int`. Gate **1314/0**, parity 16/0.

## Phase 450 — inline extensions with a LAMBDA parameter (concrete + generic receiver)  ✅
- A user `inline fun Recv.foo(f: (…)->R)` (e.g. `String.withLen(f:(String)->Int)`, `<T> T.alsoLen(f:(T)->Int)`)
  now inlines — previously the checker only pre-typed lambda args for LIBRARY extensions, so a user
  extension's lambda `it` typed as the erased `Any` and `it.length` failed → file skipped.
- The method-call resolution's `ext_lambda_pts` now falls back to the user extension's `Signature
  .lambda_param_types` (exact-receiver key), and for a GENERIC-receiver extension (keyed under `Any`)
  specializes the receiver type parameter → the actual receiver `rt` so the lambda's `it` types as the
  concrete type. The lowerer already inlines the lambda body at the `f(this)` site (lambda-arg splicing
  composes with the receiver-`this` binding from phase 448/449).
- TDD e2e `InlineExtension` extended with `String.withLen { it.length }` and `<T> T.alsoLen { it.length }`.
  Gate **1314/0**, parity 16/0.

## Phase 451 — `arrayOfNulls<T>(n)` + primitive-element collection boxing  ✅ (+3 → 1317)
- **`arrayOfNulls<T>(n): Array<T?>`** now resolves (checker `check_call`): the element is the explicit
  reference type argument; codegen allocates `new T[n]` (`b_arr_nulls`, element from the phase-446
  `synth_array_elem`). Composes with reified — `inline fun <reified T> nulls() = arrayOfNulls<T>(n)`.
- **Cross-cutting fix (`ir_lower` classpath instance-call args):** `coll.add(0)` on a PRIMITIVE-element
  collection (`ArrayList<Byte>`/`<Long>`/…) boxed the `Int` literal as `Integer`, but iterating the
  element (`checkcast Byte`/`Long`) then threw `ClassCastException`. Now, when an arg flows into an
  erased `Any`/`Object` parameter and the receiver's element type is a primitive that differs from the
  arg's type, the arg is coerced to the element primitive (`i2b`/`i2l`/…) and boxed as THAT wrapper
  (`Byte.valueOf`/`Long.valueOf`). Low blast radius (only fires for primitive-element collections with a
  type-mismatched arg). Found via `continueInFor.kt`/`continueToLabelInFor.kt` (unblocked by arrayOfNulls).
- TDD e2e `ArrayOfNullsAndPrimColl`. Gate **1317/0** (+3), parity 16/0.

## Phase 452 — String indexing `s[i]` via the `get` operator  ✅ (+3 → 1320)
- `s[i]` now type-checks as `Char`. It is the Kotlin `String.get(Int): Char` operator — added to the
  curated `resolve_string_instance` table (alongside `length`/`substring`/… — the front end deliberately
  models Kotlin's String API, NOT `java.lang.String`'s JVM methods) with `charAt` as its explicit form.
  The `Expr::Index` checker arm routes a String receiver through `resolve_string_instance("get", …)` —
  the same member-resolution path as every other String member, not an ad-hoc special-case. The backend
  already lowered `s[i]` via the `kotlin/String.get` external intrinsic (→ `charAt`), like `String.length`.
  Unblocks `this[0]` inside inline extensions on `String`.
- TDD e2e `StringIndex` (`"hi".firstChar() = this[0]`, a `for`-loop char read). Gate **1320/0** (+3),
  parity 16/0.

## Phase 453 — builtins class-member parser (de-hardcode String members, step 1)  ✅
- New `metadata::builtins_class_members(data, fqname)` reads a `.kotlin_builtins` `Class`'s declared
  MEMBERS — functions (`Class.function`=9: `name`=2, `value_parameter`=6 with `type_id`=5, `return_type_id`
  =7) resolved through the per-class `type_table` (=30) → `Type.class_name` → `QualifiedNameTable`, yielding
  Kotlin internal names (`kotlin/Int`, `kotlin/Char`, …). This is the authoritative source for a builtin
  type's API — no curated table. TDD `builtins_string_members_from_metadata` (String `get(Int):Char`,
  `plus(Any?):String`, `compareTo(String):Int`).
- **Findings driving the rest of the de-hardcoding:** (1) String's single PROPERTY (`length`) message in
  this metadata version carries ONLY flags — no name/type — so properties need separate handling.
  (2) `resolve_string_instance` CONFLATES String class members (`get`/`plus`/`compareTo`/`toString`/
  `subSequence` — builtins-sourceable, now parsed) with stdlib EXTENSIONS (`substring`/`indexOf`/
  `uppercase`/… — `StringsKt` `@Metadata` package functions, a different source). Full table removal is a
  two-source refactor: class members from `kotlin.kotlin_builtins`, extensions from `StringsKt` metadata.
- Parser added + tested; not yet wired into resolution (behavior unchanged, gate **1320/0**). Wiring the
  class-member half (incl. the `get` operator) into String resolution is the next step.

## Phase 454 — wire builtin members into resolution (de-hardcode String class members)  ✅
- Generic, type-agnostic member resolution: `LibrarySet::builtin_member_ret(internal, name, args)` (impl
  `Classpath::builtin_member_ret`) reads ANY builtin type's member return from its `.kotlin_builtins`
  (`String`, `CharArray`, `CharSequence`, … — String is not special). Property return-type fixed: a
  `Class.property` is field **10** and `Property.return_type_id` is field **9** (field 7 is the property's
  receiver) — so `length: Int` now decodes.
- `resolve.rs` String-member call sites and the `Expr::Index` arm route through
  `builtin_member_ret("kotlin/String", …)` first, falling back to the (now extension-only)
  `resolve_string_instance`. The curated String CLASS members (`length`/`get`/`charAt`) were removed —
  they come from builtins; only `StringsKt` extensions remain in the table. Gate **1320/0**.

## Phase 455 — unify builtins parsing (collection hierarchy + members, one parser/cache)  ✅
- `collection_supers` was a second `.kotlin_builtins` reader/cache duplicating the member parser's walk.
  Unified: `metadata::parse_builtins(data) -> HashMap<String, BuiltinClass{supertypes, members}>` does ONE
  walk yielding every `Class`'s supertypes AND members. `builtins_supertypes` is now a thin view over it.
- `Classpath` replaces the `collection_supers` + `builtin_members` fields with a single builtins-file cache
  (`path -> Rc<HashMap<String, BuiltinClass>>`); `is_kotlin_collection`/`kotlin_subtype`/`builtin_member_ret`
  all derive from it. Removed `parse_builtin_class` + the `CollectionSupers` alias. Gate **1320/0**.

## Phase 456 — `Unit` as a stored value (`val u = f()` where `f(): Unit`)  ✅ (+6 → 1326)
- `Stmt::Local` with a `Unit`-typed initializer no longer bails. kotlinc runs the initializer for effect
  then binds the `kotlin.Unit` singleton; the lowerer now emits `Block { stmts: [init], value: UnitInstance }`
  into a `kotlin/Unit`-typed slot — so `u.toString()`/`"$u"` yield "kotlin.Unit". TDD e2e `UnitAsValue`.
- **Survey tooling:** `KRUSTY_SURVEY_STDLIB` is now `:`-separated so the survey mirrors the gate's full
  classpath (stdlib + kotlin-test + annotations); otherwise `kotlin.test.*` shows as a false blocker
  (the assertEquals/assertFailsWith buckets vanish, survey-compiled jumps to ~1324, matching the gate).
- **Invariant fix (bound-aware erasure skip):** the `Unit` change unblocked `bridges/test23.kt`, exposing
  a separate gap — a class override whose param/return is a class type-param with a *class* upper bound
  (`class D<T : Foo> : Base<T>() { override fun bar(x: T) }`). kotlinc erases the override to the bound
  (`bar(Foo)`) and synthesizes a `bar(Object)` bridge that `checkcast`s to `Foo` (observable: CCE on an
  out-of-bound arg through the erased supertype). krusty erases the type-param to `Object`, emitting
  neither — a miscompile. The lowerer now SKIPS such an override (a principled skip, like the existing
  bound-distinct-overload rejection) until bound-aware erasure exists. Gate **1326/0**, no over-skip.

## Phase 457 — trivial elvis folding (`x ?: d` with a non-null/`null` lhs)  ✅ (+3 → 1329)
- `Expr::Elvis` no longer bails when the lhs is a non-reference primitive or the `null` literal — both are
  cases kotlinc folds at compile time (it warns "left operand is never null"/"is always null"). A
  non-reference lhs is never null, so `x ?: d` == `x` (the rhs is dead — dropped, but the lhs is still
  emitted for its side effects); a statically-`null` lhs makes the elvis always the rhs, so `null ?: d` ==
  `d`. TDD e2e `ElvisTrivial` (`42 ?: 239`, `42L ?: 239L`, `null ?: null ?: "OK"`, side-effecting lhs).
  Unblocks `elvis/nullNullOk.kt`, `primitiveTypes/kt711.kt`, `elvis/primitive.kt`. Gate **1329/0**.

## Phase 458 — arithmetic operator methods by name on primitives (`a.plus(b)`)  ✅
- `a.plus(b)`/`a.minus(b)`/`a.times(b)`/`a.div(b)`/`a.rem(b)` on a primitive numeric receiver (valid Kotlin,
  identical to `a + b`) now lowers — previously the checker typed it but the IR backend bailed. New
  `lower_prim_op_method` maps the operator-method call to the same `PrimitiveBinOp` the operator form
  produces (mixed-operand promotion + the unsigned `div`/`rem` intrinsics). Routed from the `Expr::Member`
  call arm for a primitive receiver. `Char` arithmetic methods (`c.plus(n): Char`, `c.minus(c2): Int`) added
  to the checker too (`Char` isn't `is_numeric` but maps to the operator via `check_binary`).
- **Serves the inline goal directly:** an `inline fun` body that uses operator-method syntax (`a.plus(b).times(2)`)
  now inlines and runs. TDD e2e `PrimOpMethod` (incl. an inline `combine`). Gate **1329/0** (no regression;
  the corpus files using this also have other blockers, so the count holds — the construct itself is fixed).

## Phase 459 — vacuous safe call on a non-null primitive (`a?.plus(b)`)  ✅ (+1 → 1330)
- A primitive receiver can never be null, so `a?.foo(b)` is an unnecessary safe call (kotlinc warns) ≡
  `a.foo(b)`. `Expr::SafeCall` now folds an arithmetic operator-method call on a non-reference primitive
  receiver to the plain primitive op via `lower_prim_op_method`, instead of bailing. Unblocks
  `controlStructures/kt416.kt` (`var a = 10; a?.plus(10)`). TDD: extends `PrimOpMethod`. Gate **1330/0**.

## Phase 460 — callable reference into an inline HOF (`inlineHof(::g)`)  ✅
- The inline expander only inline-expanded a function-typed argument that was a *lambda literal*; a callable
  reference (`::g`, `obj::m`) bailed. Now a non-lambda function argument is bound as a function VALUE (a
  `FunctionN`) and `f(v)` in the inlined body invokes it via `.invoke` — semantically identical (kotlinc
  inlines the reference too; the value form is box-OK and verifies, with no FunctionN-drop bookkeeping). A
  lambda literal still inlines directly. TDD e2e `InlineCallableRef` (`::g`, bound `c::m`, and a lambda in the
  same inline `apply1`). Gate **1330/0** (no regression; corpus callable-ref files have other blockers, so the
  construct is fixed without moving the count).

## Phase 461 — invoke a call result directly (`mk()()`) + named-arg eval-order guard  ✅ (+4 → 1334)
- **`mk()()` / `getFn()(x)`:** the callee of a call can now be an arbitrary expression that evaluates to a
  function value (not just a `Name`/`Member`). The callee-match catch-all lowers it to the `FunctionN` and
  invokes via `FunctionN.invoke` (same path as a function-typed local `f(args)`). Works for a plain or an
  inline producer. TDD e2e `InvokeCallResult`.
- **Invariant guard (named-argument evaluation order):** the IIFE fix unblocked the `argumentOrder/*` tests,
  exposing a pre-existing latent bug — `arg_into_params` lowers arguments in SLOT order, but Kotlin evaluates
  in SOURCE order, so a reordering named call (`f(b = …(), a = …())`) ran side effects out of order. The
  helper now detects a non-monotonic placement and skips when any reordered argument may have side effects
  (proper source-order temp-spilling is future work); pure reordered args (const/name reads) are
  order-independent and still proceed. Restores **1334/0** (was momentarily FAIL: 3).

## Phase 462 — `inline fun` with a trailing `vararg`  ✅
- The inline expander previously bailed on any `vararg` parameter. It now supports a trailing `vararg` on a
  plain (non-extension) inline fn whose element isn't a type parameter or function type: the call's trailing
  arguments are packed into a fresh array (`IrExpr::Vararg`) bound to the parameter — the same form the
  non-inline call site emits — and the inlined body iterates it. Handles the empty-vararg, fixed+vararg, and
  primitive/reference-element cases. TDD e2e `InlineVararg` (`sum(1,2,3)`, `sum()`, `join("-","a","b","c")`).
  Gate **1334/0** (construct fixed; corpus inline-vararg files have other blockers, so the count holds).

## Phase 463 — lambda param types from a function's declared return type  ✅
- A lambda that is the expression body (or a `return` value) of a function whose declared return type is a
  function type now takes its parameter types from that return type — `fun mk(): (Int) -> Int = { it + 1 }`
  types `it` as `Int`, not the erased `Object`. The checker's `check_fun_body` (expression body) and the
  `Stmt::Return` arm now route a lambda through `check_lambda_with_types(ret.params)` (the same path local
  typed-`val` initializers and HOF arguments already used). Fixes `mk()(5)`. TDD e2e `LambdaFromReturnType`
  (`inc(): (Int)->Int`, `addN(n)` via `return`, a two-param `combine`). Gate **1334/0**.

## Phase 464 — inline lambda parameter used as a value (`a(f)`)  ✅ (+1 → 1335)
- An inline fn's lambda parameter is inline-spliced only when used solely as a callee (`f(args)`). When it
  is also used as a VALUE — forwarded to another call (`a(f)`), stored, or returned — krusty now
  materializes it as a `FunctionN` (the same value-binding path as a callable-ref argument, phase 460) and
  `f(args)` invokes via `FunctionN.invoke`; previously it bailed. New `name_used_as_value` walks the body to
  distinguish a callee use from a value use. A purely-invoked lambda still splices unchanged. TDD e2e
  `InlineLambdaForwarded` (`b{…}=a(f)+1` inline→inline, `c{…}=callIt(f)*2` inline→normal). Gate **1335/0**.

## Phase 465 — `inline fun` with default-value parameters  ✅
- The inline expander bailed on any default parameter. It now fills an omitted parameter with its default
  expression (an inline fn substitutes the default directly — no `$default` method): the expander builds an
  effective per-parameter argument list (named arguments placed at their declared position, gaps filled from
  `param.default`), then binds each as before (a default lambda still splices; a default value temp-binds).
  Default + vararg combined is bailed (rare). TDD e2e `InlineDefaultParam` (`f(5)`, `f(5,1)`, `cfg(10){4}`,
  `pick(b=9)`). Gate **1335/0**.
- Known limitation (pre-existing, non-inline too): a *required* parameter that follows a *defaulted* one is
  mis-validated by the checker's trailing-`required`-count model (`map_call_args`) — `cfg(g = {…})` reports
  `x` as required. Tracked for a follow-up that records per-parameter defaults.

## Phase 466 — a required parameter after a defaulted one  ✅
- The checker's argument validation (`map_call_args`) assumed defaults were trailing — it required the first
  `required` parameters, mis-rejecting a required parameter that *follows* a defaulted one
  (`fun h(x: Int = 5, y: Int)` called `h(y = 2)`). `Signature` now records per-parameter defaults
  (`param_defaults: Vec<bool>`); `map_call_args` checks each unfilled slot against its own default (falling
  back to the `required`-prefix count only when the per-parameter info is absent). Fixes the limitation noted
  in phase 465 — `cfg(g = { … })` and `h(y = 2)` now resolve (inline and non-inline alike). TDD e2e
  `DefaultBeforeRequired` + an extra `cfg(g = {…})` case in `InlineDefaultParam`. Gate **1335/0**.

## Phase 467 — labeled local return from an inline lambda (`return@foreachT`)  ✅ (+1 → 1336)
- `return@label` was a parse error; it's the canonical inline non-local-return form (`xs.forEach { return@forEach }`).
  Now parsed (`Stmt::Return` carries an optional label), checked (a labeled return is a *local* return from
  its lambda — type-checked but not validated against the enclosing fn's return type), and lowered: when an
  inline lambda whose body contains `return@<inlineFn>` is spliced, its body is wrapped in a `while(true){…;break}`
  labeled block and a `inline_lambda_ret` frame is registered, so `return@<inlineFn>` lowers to a `break` to
  that label (acting as `continue` for the spliced loop). Modeled for `Unit`-result lambdas (the
  `forEach`/`onEach` shape); a value-result labeled return bails. A `return@enclosingFn` (no matching frame)
  falls through to the normal function return. New helpers `body_has_disallowed_return` /
  `body_has_labeled_return`. TDD e2e `InlineLabeledReturn`. Gate **1336/0**.

## Phase 468 — nested inline calls of the same fn (`a { a { 5 } }`)  ✅
- The inline recursion guard keyed on the fn NAME, so it bailed on legitimate source-level *nesting* of the
  same inline fn (`a { a { 5 } }`), not just genuine recursion. It now keys on the **call-site expression
  id**: a genuinely recursive call re-enters the *same* site (the `rec(n-1)` in `rec`'s own body) → bail;
  nesting uses *distinct* sites → allowed. (`inline_active` changed from `Vec<String>` to `Vec<u32>`.) Genuine
  recursion still skips cleanly — without the prior conservative name-bail this would expand until the same
  site recurs, so the call-id check catches it at depth 2 (no compiler stack overflow). TDD e2e `InlineNested`
  (`a{a{5}}`, 3-deep, a nested-in-a-local). Gate **1336/0**.

## Phase 469 — de-hardcode standalone `run { … }`; fix the top-level inline-splice path  ✅ (+7 → 1343)
- Removed the `if fname == "run"` checker hardcode. Standalone `run`/`with` now resolve as top-level
  `@InlineOnly inline fun`s from the classpath and splice from real stdlib bytecode through the EXISTING
  generic inline route (the same one that handles `require`/`error`) — no name match. Four root-cause fixes
  in that generic path (each helps *every* top-level inline fn, not just `run`):
  1. **Generic-return recovery** (`jvm_libraries` `resolve_callable` `@InlineOnly` top-level branch): bind the
     type variables from the arguments and recover the logical return (`run`'s `R` from the lambda's return),
     instead of the erased `Object`. A primitive result (`run { 2 + 3 }: Int`) was typing as a reference.
  2. **Spliced-result coercion** (`ir_lower`): a spliced top-level inline call's erased `Object` result is now
     coerced to the logical type (unbox/checkcast), as the member path already did.
  3. **`max_stack` for spliced lambda bodies** (`ir_emit`): the host's `max_stack` now covers the deepest
     spliced lambda body (`run { 123 != intArrayOf() as Any }` overflowed otherwise).
  4. **Inline-only lambda methods not emitted** (`IrFile::inline_only_fns`): a lambda whose body has a BARE
     (non-local) `return` is inline-only — its standalone impl method would `areturn` the enclosing fn's type
     and fail verification. The splice uses `inline_body`; the dead method is skipped. A labeled `return@x`
     (local) stays emittable. TDD e2e `ScopeRun`. Gate **1343/0**.

## Phase 470 — de-hardcode the `let`/`also` checker special-case  ✅
- Removed the `matches!(name, "let" | "also")` hardcode in the member-call checker. `let`/`also` are ordinary
  generic extension inline fns (`T.let((T) -> R): R`, `T.also((T) -> Unit): T`) — the same shape as `takeIf`,
  which already types through the generic extension-resolution path. They now type that way too (the lambda's
  `it` binds to the receiver type, the return recovered from the signature: `R` for `let`, the receiver for
  `also`), no name match. (The lowerer `let`/`also` *fallback* desugar remains for the this-capturing-lambda
  splice gap; the checker no longer name-matches them.)
- Also replaced the `matches!(name, "forEach" | "forEachIndexed")` name-match deciding whether a mutable
  variable captured by an extension lambda may be mutated (inline capture vs `Ref` box) with a generic
  `LibrarySet::extension_is_inline(receiver, name)` query — ANY inline extension's lambda is spliced, so its
  mutable captures are inline captures. De-hardcodes `forEach` AND fixes branchy `also` (`x.also { c = if … }`
  mutates `c`; without mutation-allowed the `let`/`also` checker removal would have `Ref`-boxed `c` against the
  inlined call → VerifyError). TDD e2e `ScopeFnsBranchy`. Gate **1343/0**.

## Phase 471 — de-hardcode `repeat`  ✅ (−1 → 1342, FAIL 0)
- Removed the `if fname == "repeat"` desugar in the lowerer and the `repeat_lambda` special-case in the
  checker. `repeat` is an ordinary top-level `inline fun (Int, (Int) -> Unit)` — it now types via the generic
  `toplevel_lambda_param_types` path (lambda index = `Int`, return `Unit` from its descriptor) and splices its
  real stdlib loop body via the bytecode loop-host splice, no name match.
- Generalized the mutation-allowed signal: a new `LibrarySet::toplevel_is_inline(name)` (mirrors
  `extension_is_inline`) sets `allow_lambda_mutation` for ANY inline top-level fn's lambda (it's spliced, so a
  mutable capture is an inline capture, not a `Ref` box) — replacing the `repeat`-name mutation special-case.
- Trade-off: `inline/kt66017.kt` (`forEach { repeat(size) { return "OK" } }` — a NON-LOCAL return through a
  bytecode-spliced `repeat`) now skips instead of compiling (the old in-place desugar made the return local to
  the inlined loop; the generic splice can't yet carry a non-local return through a spliced `repeat`). It SKIPS
  (FAIL 0, never miscompiles); fixing the splice to relocate a non-local return out of a spliced loop host is
  the next piece. TDD e2e covers `repeat` via the generic path (`ScopeRun`-adjacent snippets).

## Phase 472 — non-local return through a spliced loop host  ✅ (+2 → 1344)
- The bytecode splicer now carries a **non-local `return` out of a diverging spliced body**. When a lambda
  body ends in a `*return`/`athrow` (`repeat { return … }`, `forEach { … return it }`), the host's post-invoke
  continuation (a loop back-edge / exit) becomes unreachable; `splice_unified` now **synthesizes the stack-map
  frame** that position needs (host state at the invoke + the dropped `FunctionN.invoke` result), so the dead
  continuation verifies instead of a `VerifyError`.
- `lower_lambda` no longer bails on a `Nothing`-returning lambda when its body is an unconditional **non-local
  `return`** (those only splice — the impl method is inline-only, not emitted). It still bails a `Nothing`
  lambda with only a `return@label` or a `throw` (which can materialize as a real closure — that path is
  unchanged, no miscompile).
- Recovers the phase-471 `repeat` `−1` (`inline/kt66017.kt`, `forEach { repeat(n){ return } }`) **plus one more**
  diverging-non-local-return case. TDD e2e `InlineNonLocalReturnThroughLoop`. Gate **1344/0**.

## Phase 473 — non-local return from a lambda to a USER inline fn (IR inliner)  ✅
- The IR inliner (`lower_inline_fn_call`) bailed on a BARE non-local `return` in an inline lambda body
  (`body_has_disallowed_return`). It's now modeled: `lower_inline_lambda_invoke` clears the `inline_return`
  stack while lowering the spliced lambda body, so a bare `return` lowers to the REAL enclosing-function
  return (`cur_ret_ty`) rather than the inline fn's result-slot break — correct non-local-return semantics,
  even when the inline fn has its own `return`. Only `return@other` (labeled to a different inline fn) still
  bails. TDD e2e `UserInlineNonLocalReturn` (`forEachI(xs) { if (it==3) return … }`). Gate **1344/0** (no
  corpus delta — the corpus uses stdlib forms — but the construct now works, no regression/miscompile).

## Phase 474 — default LAMBDA parameter typed from its function type  ✅
- A parameter default that is a lambda for a function-typed parameter (`g: (Int) -> Int = { it + 1 }`) was
  checked with no expected type, so its `it` typed as the erased `Object` → `it + 1` errored. The default-arg
  check (`check_fun_body`'s default loop) now routes a lambda default through `check_lambda_with_types` with
  the parameter's declared lambda parameter types (`p.ty.fun_params`), as typed local / HOF-argument lambdas
  already are. TDD e2e `DefaultLambdaParam`. Gate **1344/0**.
- (Noted while probing: a GENERIC extension inline fn with a capturing lambda — `<T> T.alsoLog { … }` — has a
  pre-existing frame bug, gate-green/uncovered by the corpus; non-generic ext + plain inline + mutable capture
  all verify. Tracked separately from this session's work.)

## Phase 475 — generic-receiver extension inline: specialize the lambda param from the receiver  ✅
- A generic-receiver extension inline fn (`inline fun <T> T.applyIt(f: (T) -> R)`) inlined its lambda with the
  parameter typed by the erased `Object`, not the actual receiver type — so `it.length` (a `String` member)
  failed to resolve and other uses risked an erased-frame mismatch. `lower_inline_fn_call` now binds the type
  parameter to the SPECIALIZED receiver type (`tbinds[receiverTypeParam] = recv_ty`), so a lambda parameter
  typed by it specializes (`it: String`), matching the checker. TDD e2e `GenericReceiverExtInline`
  (`"abc".applyIt { it.length }`, `list.applyIt { it.size }`). Gate **1344/0**.
- (Still latent — documented: a generic-receiver ext inline whose lambda also CAPTURES a variable
  (`<T> T.alsoLog { capture }`) hits a deeper multi-slot erasure-frame `VerifyError` — the receiver-`this`
  slot typing + `Ref`-box-for-user-inline-ext interaction — beyond this lambda-param specialization. Not in
  the box corpus.)

## Phase 484 — multiple branchy lambda splices per method (per-statement operand-stack reset)  ✅
- Two `takeIf`/`takeUnless`-with-elvis in ONE method bailed: a branchy lambda splice (`takeIf`'s predicate
  inlined into a body with an internal join) tracks its branches only approximately, leaving the emitter's
  `cur_stack` drifted ABOVE the real (verified-balanced) height; a LATER branchy splice then saw a non-empty
  baseline (`needs_frames && stack != 0`) and bailed. (Before phase 479 it was a wrong-but-compiling
  miscompile; phase 479's live elvis turned it into a bail.)
- Fix in the emitter's two `Block` arms (`emit`/`emit_value`): a statement nets zero on the operand stack
  (its value is stored/discarded), so after each statement reset `cur_stack` to the pre-statement baseline.
  This undoes any approximate-splice drift without affecting `max_stack` (already updated during the splice).
- TDD: `TakeIfNullableResult` rewritten to put all six `takeIf`/`takeUnless`-elvis cases in ONE `box()` (the
  previously-bailing shape). Gate **1352/0**; full suite green.

## Phase 483 — chained safe-call scope fns over a nullable-primitive result (`s?.let{}?.let{it+1}`)  ✅
- A chained `s?.let { it.length }?.let { it + 1 }` mistyped the second `it` as the boxed `java/lang/Integer`
  (the first `?.let`'s `Int?` result), so `it + 1` failed (`operator … 'java/lang/Integer' and 'Int'`). Inside a
  safe-call scope fn the receiver is NON-null, so a nullable-primitive receiver now binds `it`/`this` as the
  UNBOXED primitive: `safe_scope_call_result` unwraps the wrapper for the lambda typing, and
  `lower_scope_inline_on` unboxes the receiver value (`Integer`→`int`) before binding the slot.
- TDD: `SafeCallScopeFn` extended with chained `?.let`/`?.run` over primitive results + null short-circuit.
  Gate **1352/0**.

## Phase 482 — no-lambda `@InlineOnly` extensions on a primitive receiver (`Char.isDigit()` etc.)  ✅
- `'a'.isDigit()`/`isLetter()`/`uppercaseChar()`/… bailed (`unresolved method on Char`): they're no-lambda
  `@InlineOnly` extensions, which the checker only accepted for *lambda* args (`takeIf`). The checker now also
  accepts a no-lambda `@InlineOnly` extension, but TIGHTLY scoped so the generic splice stays value-correct:
  a **non-unsigned primitive receiver**, **primitive/`String` return**, **no function-type parameter**, and
  `can_inline_call` (the body actually splices). No name match — the receiver/return SHAPE selects it.
- A broad widen first miscompiled 13 corpus cases (committed as a documented anti-pattern in a249c98); the
  failure modes were: a function-typed param (`let`/`apply` fallback with a non-literal arg → private-method
  `IllegalAccessError`), a multi-step reference body (`StringBuilder.appendLine` → wrong value), and an
  unsigned return. The shape gate excludes the first two; the last needed `LibrarySet::metadata_return_unsigned`
  (reads the Kotlin return class from `@Metadata`) — `Int.toUShort(): UShort` erases to a signed `Short` in the
  JVM signature, so `Ty` alone can't tell `40000` from `-25536`; such an extension is now skipped (a clean
  bail, not a miscompile — krusty's unsigned support is incomplete).
- TDD e2e `PrimitiveInlineExtension` (`isDigit`/`isLetter`/`isWhitespace`/`uppercaseChar`/`lowercaseChar`,
  `"aBc".map { it.uppercaseChar() }`). Gate **1352/0** (+5 corpus cases).

## Phase 481 — safe-call stdlib EXTENSION calls + chains (`s?.uppercase()?.length`)  ✅
- The safe-call lowerer resolved only members (`resolve_method`/`resolve_instance`); a stdlib extension via a
  safe call (`s?.uppercase()`) bailed, which also broke any chain through it (`s?.uppercase()?.length`). The
  classpath-instance-method branch now falls back to `lower_ext_call_on(recv2, …)` (the shared extension path
  from phase 477) when `resolve_instance` finds no member — inlining the extension on the non-null receiver.
- TDD: `SafeCallScopeFn` extended with `s?.uppercase()`, `s?.uppercase()?.length`, and the null-receiver
  short-circuit. Gate **1347/0**.

## Phase 480 — safe-call scope functions `s?.let`/`?.run`/`?.also`/`?.apply`  ✅
- The most idiomatic null-handling form bailed (`unresolved member 'let' on 'String'`): three gaps —
  - Parser: a trailing lambda after a safe call (`s?.let { … }`) was wrapped in an OUTER call
    (`(s?.let)(lambda)`); now it attaches as the safe call's argument (`SafeCall` arm in the trailing-lambda
    postfix), appending after any `(…)` args.
  - Checker: `safe_scope_call_result` types `s?.scopeFn { … }` like the non-safe form (binds `it`=receiver for
    `let`/`also`, `this`=receiver via `check_with_receiver` for `run`/`apply`); `let`/`run` → the lambda body,
    `also`/`apply` → the receiver. The existing safe-call tail then wraps it nullable.
  - Lowerer: `lower_scope_inline_on` inlines a scope fn over an already-lowered receiver value (binds `it`/
    `this`, lowers the body, yields body or receiver); `lower_safe_scope_member` drives it in the safe call's
    non-null branch (reusing `recv2` = the non-null temp), so the surrounding null-check + nullable-wrap make
    `s?.…` yield `null` when `s` is null.
- TDD e2e `SafeCallScopeFn` (`let`/`run`/`also`/`apply`, null + non-null receiver, primitive/reference/user
  results). All verify under `-Xverify:all`. Gate **1347/0**.
- (Separate gap, still open: a chained safe-call EXTENSION — `s?.uppercase()?.length` — bails; the safe-call
  path doesn't yet resolve stdlib extension calls. Never miscompiles.)

## Phase 479 — `takeIf`/`takeUnless` nullable result (fixes the dropped-elvis NPE miscompile)  ✅
- `5.takeUnless { it > 3 } ?: 0` threw `NullPointerException` (and `takeIf` was wrong whenever the predicate
  selected the null branch): `takeIf`/`takeUnless` return `T?`, but that nullability lives only in `@Metadata`
  (the JVM `Signature` erases it), so the result typed as a non-null primitive `Int`. The elvis lowering folds
  `x ?: d` to `x` for a non-reference (never-null) lhs (resolve `Expr::Elvis`, `!lty.is_reference()`), dropping
  the null-check and unboxing a possibly-null value → NPE.
  - Metadata: `parse_function` now reads the return `Type.nullable` flag (`parse_type_nullable`);
    `package_function_return_nullable` + `Classpath::metadata_return_nullable` expose it (facade-part merged,
    cached).
  - `extension_callable`: when the resolved scope fn's metadata return is nullable AND the logical return is a
    primitive, type the result as the boxed wrapper (`Int` → `java/lang/Integer`) — a reference, so the elvis
    keeps its null-check. The spliced body already yields a boxed-or-null value, so the type now matches.
- TDD e2e `TakeIfNullableResult` (`takeIf`/`takeUnless`, predicate true/false, primitive + reference receiver,
  nullable-typed binding). Gate **1347/0**.
- (Found + documented, separate emitter limitation: TWO branchy lambda splices each wrapped in an elvis in the
  SAME method — `val a = x.takeIf{}?:d; val b = y.takeIf{}?:d` — bails cleanly. The emitter's `cur_stack`
  tracker drifts +1 after a branchy lambda splice (its internal branches aren't linearly modelled — the single
  case still VERIFIES, so the real stack is balanced; only the tracker is approximate), so the second splice
  falsely sees a non-empty baseline. Was a wrong-but-compiling miscompile before this fix; now a safe bail.
  Next target: accurate post-branchy-splice stack tracking.)

## Phase 478 — `with(x) { … }` + checker-driven receiver-lambda lowering (closes the `with` bail)  ✅
- `with(x) { … }` (the stdlib 2-arg receiver-lambda scope fn) bailed in the lowerer — only `x.run`/`x.apply`
  were inlined (name-matched in the backend). Generalized via a checker→lowerer side table so the receiver-
  lambda decision lives once in the checker and the backend is name-match-free:
  - `TypeInfo.receiver_lambdas: HashMap<ExprId, ReceiverLambda>` — the checker records each resolved
    receiver-lambda scope call (`x.run`/`x.apply`/`with(x)`) as `{ receiver, body, returns_receiver }` at the
    `check_with_receiver` sites.
  - Lowerer: a new `lower_receiver_lambda` evaluates the receiver into a fresh `this`-bound slot, lowers the
    body (with `cur_class` cleared so members resolve through the implicit-`this` paths), and yields the body
    (`run`/`with`) or the receiver (`apply`/`also`). Driven by the table via a guarded `Expr::Call` arm —
    `run`/`apply` no longer name-matched in the backend.
- TDD e2e `WithReceiver` (`with` over builtin/classpath/user receivers; member read, member call, stdlib
  extension call; nested `"xy".run { with(this) { length } }`). All verify under `-Xverify:all`. Gate **1347/0**
  (+1 corpus case).
- (Still open: `x.run { run { … } }` — a NESTED *top-level* `run` whose plain `()->R` lambda captures the outer
  `this` — bails cleanly; needs `this`-capturing closures. The common nested forms via an explicit receiver —
  `run { with(this) { … } }`, `apply { v = with(x) { … } }` — work.)

## Phase 477 — bare stdlib-EXTENSION calls through the implicit `this` (closes the receiver-lambda bail)  ✅
- A bare extension call in a receiver-lambda body / extension-fn body (`"ab".run { uppercase() }`,
  `fun String.shout() = uppercase()`) bailed: `uppercase`/`reversed` are stdlib EXTENSIONS (`StringsKt`),
  not `java.lang.String` members, so the member-only implicit-`this` resolution missed them.
  - **Lowerer**: new `lower_ext_call_on` resolves an extension call on a lowered receiver value through the
    library reader — a public extension (`invokestatic facade.name(recv, args)`), then a private
    `@InlineOnly` one whose real body the backend splices (`String.uppercase()` → `toUpperCase(Locale.ROOT)`,
    `reversed()` → `StringBuilder(this).reverse()`). A new `lower_this_member_call` tries, in order, a user
    instance method, a builtin/library member, then an extension — and is invoked **before** the
    receiver-less top-level-function branch (gated on `cur_class.is_none()` + a `this` slot), so the implicit
    receiver wins, matching Kotlin scoping. This was the bug behind `reversed()` mis-resolving to the
    top-level `ArraysKt.reversed` (→ `[]`); now it picks `CharSequence.reversed`.
  - **Checker**: `this_member_call_ret` gains receiver-aware extension resolution
    (`resolve_callable(name, Some(rt), …)`) so the call is typed by the right overload, not the receiver-blind
    fallthrough that picked `Iterable.reversed → List`.
  - The three duplicated implicit-`this`-call branches (user method / library member / extension) collapsed
    into the single `lower_this_member_call`.
- This also fixes the pre-existing gap where a bare extension call in an ordinary extension-fn body
  (`fun String.shout() = uppercase()`) didn't compile.
- TDD e2e: `ReceiverLambdaAnyReceiver` extended with `uppercase()`/`trim()`; new `ExtensionFnBodyBareExtCall`
  (`shout`/`echo` composing `uppercase`/`reversed` through implicit `this`). All verify under `-Xverify:all`.
  Gate **1346/0**.

## Phase 476 — receiver lambdas (`run`/`apply`) over ANY receiver + de-hardcode `String.length`  ✅
- A receiver lambda's `this` is the receiver, so a bare member in the body resolves against it. Previously
  this only worked for a USER-class receiver (`check_with_receiver` rejected anything else with "must be a
  class instance", and the lowerer bailed when the receiver wasn't a reachable user class). Generalized to
  ANY receiver type — a builtin (`String`), a library type (`List`), a classpath class (`StringBuilder`):
  - **Checker**: `check_with_receiver` drops the `Ty::Obj`-only gate (binds `this_ty` for any receiver); the
    bare-name READ arm gains a `try_member_read` fallback (a non-erroring `check_member` probe via a diag
    snapshot/truncate) so `length` resolves as `this.length`; a new `this_member_call_ret` resolves an
    unqualified CALL (`append(x)`/`uppercase()`) as a member of `this_ty` (builtin/library/user), mirroring
    the qualified `recv.m(args)` typing.
  - **Lowerer**: the receiver-lambda path no longer requires a user-class receiver; a new
    `lower_member_read_on` resolves a bare `this.name` READ generically through the classpath reader (so
    `String.length` lowers as `java/lang/String.length()` — the same generic `resolve_instance` path as
    `uppercase()`, NOT a hardcoded `name == "length"`); and a new branch resolves a bare `this.m(args)` CALL
    on a builtin/library receiver via `resolve_instance`. The qualified `Member`-read arm was refactored to
    route through `lower_member_read_on` too, so the old hardcoded `String.length`/cross-file/library tail is
    one shared generic helper.
  - AST: `TypeRef.fun_has_receiver` marks a receiver function type `Recv.(A)->R` (the receiver still folds
    in as `fun_params[0]`; the flag records it for future receiver-lambda type-directed inference).
- TDD e2e `ReceiverLambdaAnyReceiver` (`"ab".run { length }`, `listOf(1,2,3).run { size }`,
  `StringBuilder().apply { append("O"); append("K") }`, `C(1).apply { bump(); bump() }`, `5.run { this+1 }`)
  — all run OK under `-Xverify:all`. Gate **1346/0** (picked up 2 corpus cases).
- (Still open: a bare stdlib-EXTENSION call through the implicit `this` — `"ab".run { uppercase() }` —
  bails cleanly; `uppercase` is `StringsKt.uppercase` (an extension), not a `java.lang.String` member, so it
  needs the extension-resolution path, not `resolve_instance`. Never miscompiles.)

### Working agreements
- Every phase: `cargo test` green before moving on; no `unwrap` on user-input paths in the driver.
- Keep the AST/IR **index-based** (no `Box`/`Rc` graphs) — that's the experiment.
- Record every Kotlin-semantics decision (overflow, division, concat order) in SPEC §7 with a test.
- The harness is the source of truth for "correct"; don't claim a feature works without a diff test.
