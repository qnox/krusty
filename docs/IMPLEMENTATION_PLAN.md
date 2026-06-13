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
### 4e — v52 + StackMapTable ⬜ (hardening, for exact version match with kotlinc)

## Phase 4b — `@kotlin.Metadata` emitter (protobuf)  🚧 (load-bearing for Kotlin-library ABI)
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
### 6e — `java.lang.String` instance methods ✅
- ✅ `resolve_string_instance` (curated `java.lang.String` subset: `length`/`isEmpty`/`substring`×2/
  `indexOf`/`concat`) drives typecheck + `invokevirtual` codegen. Interim until jimage gives the
  full JDK; each entry matches what kotlinc emits.
- ✅ **Differential pass**: `tests/diff_kotlinc.rs` now includes `s.substring(1)`, `s.substring(1,3)`,
  `s.indexOf("b")` — krusty's bytecode + execution match kotlinc exactly. Unit tests in `resolve.rs`.
### 6c — minimal Java *source* front end ⬜ (signatures only, for mixed kt+java)
### 6d — scale benchmark ⬜ (peak RSS vs kotlinc on many_functions/multifile)

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

## Phase 7 — Hardening  ⬜
- Fuzz the lexer/parser; property tests for arithmetic semantics vs a reference evaluator.
- Expand the subset opportunistically (when/nullable) only if it serves the memory thesis.

---

### Working agreements
- Every phase: `cargo test` green before moving on; no `unwrap` on user-input paths in the driver.
- Keep the AST/IR **index-based** (no `Box`/`Rc` graphs) — that's the experiment.
- Record every Kotlin-semantics decision (overflow, division, concat order) in SPEC §7 with a test.
- The harness is the source of truth for "correct"; don't claim a feature works without a diff test.
