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

## Phase 7 — Hardening  ⬜
- Fuzz the lexer/parser; property tests for arithmetic semantics vs a reference evaluator.
- Expand the subset opportunistically (when/nullable) only if it serves the memory thesis.

---

### Working agreements
- Every phase: `cargo test` green before moving on; no `unwrap` on user-input paths in the driver.
- Keep the AST/IR **index-based** (no `Box`/`Rc` graphs) — that's the experiment.
- Record every Kotlin-semantics decision (overflow, division, concat order) in SPEC §7 with a test.
- The harness is the source of truth for "correct"; don't claim a feature works without a diff test.
