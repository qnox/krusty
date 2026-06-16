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

## Phase 7 — Hardening  ⬜
- Fuzz the lexer/parser; property tests for arithmetic semantics vs a reference evaluator.
- Expand the subset opportunistically (when/nullable) only if it serves the memory thesis.

---

### Working agreements
- Every phase: `cargo test` green before moving on; no `unwrap` on user-input paths in the driver.
- Keep the AST/IR **index-based** (no `Box`/`Rc` graphs) — that's the experiment.
- Record every Kotlin-semantics decision (overflow, division, concat order) in SPEC §7 with a test.
- The harness is the source of truth for "correct"; don't claim a feature works without a diff test.
