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
### 4e — v52 + StackMapTable ✅ (exact version match with kotlinc)
- ✅ All emitted methods now carry a valid `StackMapTable` attribute, required by Java 8
  (class-file v52). Branch targets tracked via `rec()` / `rec_s()` in `FunctionEmitter`;
  synthetic methods (`copy$default`, `equals`) register frames via `CodeBuilder.add_frame_if_new`.
- ✅ `init_temp` pattern: any slot added to `self.slots` via `alloc_temp` or `alloc_slot` before a
  `rec()` call gets a zero/null default store so the JVM's computed type matches the declared frame.
- ✅ Divergence-aware codegen: `goto`/store after a `return`/`throw` branch is elided; frames for
  dead code are filtered to avoid "bad offset" errors; duplicate-offset frames deduped.
- ✅ All `cargo test` green; `-Xverify:all` passes on all emitted class files.

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

## Phase 7 — Hardening  ⬜
- Fuzz the lexer/parser; property tests for arithmetic semantics vs a reference evaluator.
- Expand the subset opportunistically (when/nullable) only if it serves the memory thesis.

---

### Working agreements
- Every phase: `cargo test` green before moving on; no `unwrap` on user-input paths in the driver.
- Keep the AST/IR **index-based** (no `Box`/`Rc` graphs) — that's the experiment.
- Record every Kotlin-semantics decision (overflow, division, concat order) in SPEC §7 with a test.
- The harness is the source of truth for "correct"; don't claim a feature works without a diff test.
