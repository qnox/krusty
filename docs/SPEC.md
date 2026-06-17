# krusty — a memory-lean Kotlin→JVM compiler PoC

**Status:** PoC / experiment. NOT a production Kotlin compiler.
**Goal:** demonstrate that a **linear, data-oriented, per-file streaming pipeline** compiles a
useful subset of Kotlin to JVM bytecode with a **working-set bounded by a single file**, instead of
the whole-module FIR+IR graph that makes `kotlinc` memory scale with module size.

This project is the concrete follow-up to the memory investigation in
`~/projects/kotlin-memory-bench` (see `COMPARISON_REPORT_2.4.0.md`): localized tuning of kotlinc
caps at ~8% on full compilation because the pipeline is whole-module; per-file processing measured
~80% lower peak. krusty *is* the per-file pipeline, built from scratch where there's no legacy
whole-module architecture or plugin contract to fight.

---

## 1. Design thesis

- **Linear pipeline, vertical execution.** Parse-all-signatures (cheap, global) → then per file:
  `typecheck body → lower → emit .class → drop`. At most one file's bodies/IR are live.
- **Data-oriented representation.** AST and IR are **structs-of-arrays indexed by `u32`**, not a
  pointer graph of boxed nodes. Spans, types, and symbols live in parallel arenas. This is the
  Zig/Carbon/rust-analyzer style — the opposite of kotlinc's `Fir*` object graph (~38M objects on
  a real build). Cache-friendly, header-free, bulk-freeable.
- **No GC, arena lifetimes.** Per-file arenas are dropped wholesale after the file is emitted.
- **Correctness by differential testing**, not by reimplementing kotlinc's exact output (§6).

## 2. Scope (what the PoC compiles)

### v0 supported Kotlin subset
- A single package; multiple `.kt` files compiled together.
- **Top-level functions**: `fun name(p: T, ...): R = expr` and block bodies `{ ... }`.
- **Types**: `Int`, `Long`, `Boolean`, `Double`, `String`, `Unit`. (No generics, no nullable types in v0.)
- **Expressions**: integer/double/boolean/string literals; arithmetic (`+ - * / %`), comparisons
  (`< <= > >= == !=`), boolean (`&& || !`), string `+` concat; parenthesization; calls to other
  top-level functions in the compilation; `if/else` as expression and statement.
- **Statements**: local `val`/`var` with inferred or explicit type; assignment; `return`; `while`.
- **Member calls** limited to a hardcoded JDK surface needed by tests (`Int.toString()`,
  `String` concat, `println`) — see §5.

### Explicit non-goals (v0)
Classes/objects/interfaces, generics, nullability & null-safety, lambdas/inline, extension
functions, properties with backing fields, `when`, smart casts, coroutines, multiplatform,
annotations/`@Metadata`, reflection, **all compiler plugins**, real Java-source parsing, incremental
compilation. Java *interop* in v0 = referencing a small fixed set of JDK class signatures (§5),
**not** compiling `.java`.

> Rationale: this subset covers the `kotlin-memory-bench` scenarios (`many_functions`, `multifile`,
> `bodyheavy`) — the exact workloads where the per-file pipeline showed ~80% lower peak — so krusty
> can be benchmarked head-to-head with kotlinc on identical inputs.

## 3. Pipeline (linear, per-file streaming)

```
                 ┌── global (cheap) ──┐      ┌──────── per file, streamed ────────┐
 source files →  lex → parse → collect →  for each file:  typecheck → lower → emit → DROP arena
                          (AST)   signatures                 (types)    (IR)   (.class)
```

- **Stage A — Lex** (`lexer`): byte slice → token stream. No allocation per token beyond a `Vec`.
- **Stage B — Parse** (`parser`): tokens → AST in an arena (`ast`). One arena per file; nodes are
  `u32`-indexed records in parallel `Vec`s.
- **Stage C — Collect signatures** (`resolve::sigs`): walk each file's top-level decls, record
  `(name, param types, return type)` into a **global symbol table**. Cheap; no bodies touched.
- **Stage D — Per file**:
  - **typecheck** (`resolve::check`): resolve names against the global table + locals, assign a
    `TypeId` to every expression, report diagnostics.
  - **lower** (`ir`): AST → a tiny stack-oriented IR (or straight to a bytecode builder).
  - **emit** (`codegen`): IR → JVM `.class` bytes via a hand-written class-file writer.
  - **drop**: the file's AST/IR/typecheck arenas are freed before the next file. ← the memory win.

Peak memory ≈ `global signature table` + `one file's AST+IR` + `fixed runtime`, i.e. ~constant in
file count, vs kotlinc's linear growth.

## 4. Crate layout

```
src/
  main.rs        # CLI driver: discover files, run the linear pipeline
  lexer.rs       # Stage A
  token.rs       # token kinds + spans
  ast.rs         # arena AST (SoA, u32 NodeId)
  parser.rs      # Stage B (recursive descent / Pratt for expressions)
  types.rs       # TypeId, primitive type table
  resolve.rs     # Stage C (signatures) + Stage D (typecheck)
  ir.rs          # tiny IR
  codegen/
    classfile.rs # JVM class-file writer (constant pool, methods, Code attr)
    emit.rs      # IR → bytecode
  diag.rs        # diagnostics (spans + messages)
  driver.rs      # orchestrates the streaming pipeline + arena drop points
harness/         # differential test harness (vs kotlinc) — see §6
tests/cases/     # .kt programs + expected behavior
docs/            # this spec + the implementation plan
```

## 5. Java / JDK interop (v0)

Real `.java` parsing and `.class` signature reading are deferred. v0 hardcodes a minimal
**builtin signature table** for the JDK symbols the test programs need:
- `java.lang.String` (concat, `length`), `java.lang.Integer.toString(int)`,
  `java.lang.System.out` + `java.io.PrintStream.println(...)`, `java.lang.Object`.
Kotlin `Int.toString()` etc. map to these via a small intrinsics table. Phase 5 (plan) replaces
this with a real `.class` reader (`cafebabe`/hand-rolled) so any JDK/Java dependency works, and
Phase 6 adds a minimal Java *source* front end for mixed compilation.

## 6. Correctness & compatibility: differential testing vs kotlinc

**Compatibility IS a goal — specifically ABI + `@Metadata`, NOT byte-identity.** A krusty-compiled
`.class` must be usable as a drop-in library by Kotlin and Java consumers. That requires matching
the *contract* kotlinc produces, not the exact bytes:

- **Why not byte-identity:** kotlinc itself isn't byte-stable across versions (constant-pool order,
  `invokedynamic` vs `StringBuilder` concat, line tables, synthetic shapes). Byte-identity is
  unachievable *and* unnecessary — binary compatibility doesn't depend on it.
- **What IS required for library compatibility:**
  1. **ABI identity (exact).** Public class names + file→class mapping (top-level funs → `<File>Kt`),
     method/field **descriptors**, **modifiers/flags**, name mangling, `$default` methods for default
     args, `$annotations`/synthetic accessors. Consumers link against *this*; it must equal kotlinc.
  2. **`@kotlin.Metadata` equivalence (semantic).** A Kotlin consumer reads the protobuf-encoded
     `@Metadata`, not the raw signatures, to recover the Kotlin API (nullability, `val`/property vs
     method, default values, named params, variance). krusty must emit `@Metadata` that **decodes to
     the same Kotlin declarations** as kotlinc, with a compatible `metadataVersion`. (Semantic
     equivalence of the decoded protobuf — byte-identity of the annotation not required.)

Correctness/compat layers, strongest first (1–2 are the **primary gate** for library output):

1. **ABI diff (primary).** Parse both outputs' public members (names, descriptors, modifiers) and
   require an **exact** match. Any difference is a compatibility break.
2. **`@Metadata` diff (primary).** Decode `@kotlin.Metadata` from both (documented
   `kotlin-metadata-jvm` schema) and compare the recovered declarations; require semantic equality
   + compatible version.
3. **Execution differential.** Compile with both krusty and reference kotlinc (`kotlin-compiler`
   2.4.0 jar in `~/.m2`, headless); run a generated driver calling the functions with fixed inputs;
   compare results. Verifies behavior independent of code-gen shape.
4. **Structural disassembly (informational).** `javap -c -p` normalized; flags *how* code differs
   (e.g., concat strategy). Not a gate — shape may legitimately differ.
5. **Verifier (always).** Every `.class` must pass `java -Xverify:all`; non-verifying = fail.

The harness (`harness/`) is a Rust integration test shelling out to the reference compiler,
`javap`/a class-file parser, and `java`. Edge-case suite (§7) lives in `tests/cases/`.

## 7. Edge cases tracked (grow as implemented)

- Integer overflow / wraparound semantics (Kotlin `Int` is 32-bit two's complement).
- Integer division/modulo by constants; `/` truncation toward zero; `%` sign.
- `Long` vs `Int` literal typing and promotion; `Double` arithmetic & NaN comparisons.
- String concat of mixed types (`Int + String`, `Boolean + String`) and evaluation order.
- `if`-as-expression typing (common supertype) and as-statement (Unit).
- Operator precedence/associativity vs Kotlin grammar (Pratt table must match).
- `==` on `String` (Kotlin `==` = `.equals`, `===` = reference) — v0 supports `==` only.
- Boolean short-circuit evaluation (`&&`/`||`) side-effect order.
- Function call argument evaluation order; recursion.
- Shadowing of locals; `val` reassignment is an error.
- Empty file; file with only signatures; forward references between top-level functions.
- `data class`: `equals`/`hashCode`/`toString`/`componentN` are synthesized (in IR lowering, so all
  backends share them). `equals` compares field-wise with IEEE-aware `Double/Float.compare` and
  structural reference equality; `hashCode` is the `31*result + fieldHash` fold; `toString` is
  `Class(p1=v1, p2=v2)`. `copy` (default arguments) is not yet supported — such files are skipped.
- `enum class`: compiled as a `final` class extending `java/lang/Enum` with a `public static final`
  constant per entry, a synthetic `$VALUES` array, a private `(String name, int ordinal, …userArgs)`
  constructor calling `super(name, ordinal)`, a `<clinit>` that constructs entries in declaration
  order, and synthetic `values()`/`valueOf(String)`. `e.ordinal`/`e.name` are `Enum.ordinal()`/
  `name()`; entry equality is reference identity (`==`). Entry constructor args are constant
  expressions evaluated in `<clinit>` (branchy args, entry bodies, and abstract enum methods are not
  yet supported — krusty skips such files rather than miscompile).
- Explicit builtin operator-methods on numeric primitives: `a.plus(b)` ≡ `a + b` (same promotion);
  `a.compareTo(b)` uses IEEE total order (`{Integer,Long,Float,Double}.compare`, so
  `0f.compareTo(-0f) == 1`, `Double.NaN.compareTo(x) == 1`). Kotlin routes the *infix* form
  `a rem b` to a user `operator`/`infix` extension but the *dot* form `a.rem(b)` to the builtin;
  krusty can't tell them apart in the AST, so it skips when such a user extension exists
  (`tests/cases`/box `infixFunctionOverBuiltinMember.kt`). `mod`/`rangeTo`/`inc`/`dec` unsupported.
- Safe call `a?.b` / `a?.m(args)`: evaluates the receiver once into a temp, then yields the member
  access (property `GetField` / method `MethodCall`) when the temp is non-`null`, else `null` — i.e.
  `{ val t = a; if (t != null) t.b else null }`. Lowered in the front-end so every backend shares it;
  composes with Elvis (`a?.m() ?: d`). The merge of the member arm (a reference) with the `null` arm
  types the verification stack as the member's reference type (`null` is assignable to any reference),
  not `top` — emitting a `top` there is a `VerifyError: Bad type on operand stack`. Only user-defined
  member targets are resolved; safe calls on stdlib receivers (`s?.substring(1)`) need the external-call
  path and are skipped (`tests/safe_call_e2e.rs`).
- Lambdas `{ a, b -> … }`: a function type `(A,…) -> R` is the JVM interface
  `kotlin/jvm/functions/Function{arity}`. A non-capturing lambda compiles to `invokedynamic` bound by
  `LambdaMetafactory.metafactory` to a synthesized `private static` method `<enclosing>$lambda$<n>`
  holding the body (with the lambda's real parameter types). The `implMethod` is primitive-specialized
  (`box$lambda$0(I)I`) while the `instantiatedMethodType` is boxed (`(Integer)Integer`), so the
  metafactory inserts the box/unbox adapter — matching kotlinc 2.x. Calling a function value `f(args)`
  goes through `FunctionN.invoke` (`(Object…)Object`): arguments are boxed, the result cast/unboxed to
  the return type. Only non-capturing lambdas returning a concrete non-`Unit` type, passed to a
  non-generic function, are supported; capturing lambdas, `Unit`/`Nothing` lambdas (need the
  `kotlin/Unit` singleton), lambdas inside class methods, and generic/suspend consumers are skipped
  (`tests/lambda_e2e.rs`, `tests/indy_infra_e2e.rs`).
- Unbound top-level function references `::foo`: same `invokedynamic`/`LambdaMetafactory` lowering as a
  lambda, but the impl method handle points directly at the referenced function (no synthesized body).
  kotlinc instead emits a `kotlin/jvm/internal/FunctionReferenceImpl` subclass carrying reflection
  metadata, but that class is synthetic and not part of the facade's ABI, so public signatures and the
  round-trip result match. A function type lowers to the backend-neutral `IrType::Function`; the **JVM
  backend** maps it to `kotlin/jvm/functions/FunctionN` and enforces the JVM-only fixed-arity limit
  (`Function0..22`) — higher arities, and bound/object/constructor references, are skipped
  (`tests/callable_ref_e2e.rs`).
- Not-null assertion `x!!`: yields `x`, throwing a `NullPointerException` if it is null. Compiled (on a
  reference operand) as `dup` + `kotlin/jvm/internal/Intrinsics.checkNotNull(Object)V` — the value
  stays on the stack and the duplicate is consumed by the check, matching kotlinc. On a non-null
  primitive operand it is a no-op (`tests/not_null_assert_e2e.rs`).
- Constructing a classpath (non-IR) class (`RuntimeException("x")`, an imported Java type): `new` +
  `dup` + arguments + `invokespecial <init>`, with the constructor descriptor resolved from the
  classpath. JDK `Throwable` types fall back to the `()`/`(String)` constructors (the classpath reader
  doesn't read jimage constructor descriptors yet, so classes whose `<init>` lives only in the jimage —
  e.g. `StringBuilder` — are skipped). `throw e` emits `athrow` (`tests/throw_e2e.rs`).

## 8. Success criteria for the PoC

1. krusty compiles the `kotlin-memory-bench` `many_functions` / `multifile` / `bodyheavy` programs.
2. **ABI match:** public members (names/descriptors/modifiers) are identical to kotlinc's output.
3. **`@Metadata` match:** emitted metadata decodes to the same Kotlin declarations as kotlinc
   (compatible `metadataVersion`), so output is consumable as a Kotlin library — verified by having
   kotlinc itself compile a consumer against krusty's output.
4. **Behavior match:** execution-differential tests pass on the §7 edge cases.
5. Measured peak RSS compiling `bodyheavy` is **bounded ~constant in file count** and well below
   kotlinc's (the per-file thesis, on a real implementation).
6. All emitted classes pass the JVM verifier.

> Note: criteria 2–3 are the load-bearing compatibility goals; byte-identity is explicitly out.
> The ultimate compat test (criterion 3) is **round-trip**: compile a library with krusty, then
> compile a *Kotlin consumer* of it with real kotlinc — if kotlinc accepts krusty's `@Metadata` and
> resolves the API, the output is a genuine Kotlin library.
