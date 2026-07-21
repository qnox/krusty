# krusty byte-identity — session handoff (uncommitted work on branch `feat/metadata-byte-exact`)

> Temporary handoff note (untracked, delete after reading). Goal: krusty emits byte-for-byte
> identical `.class` output to kotlinc 2.4.0 for all of infragnite. Written from an infragnite-anchored
> session whose commit hook blocked committing here — hence this file instead of memory.

## Current state (all on disk, UNCOMMITTED, all gates green)

`git status` shows ~10 modified files + `tests/data_class_metadata_wiring_e2e.rs`. Diffstat ≈ +360 lines.

### ✅ MILESTONE: `class C(val x: Int)` is FULLY BYTE-IDENTICAL to kotlinc (603B, `cmp` clean)

First complete byte-for-byte identical class. The constant-pool interning-order barrier (below) was
closed via pool seeding + metadata/finish interning reorders. Durable guard:
`tests/data_class_metadata_wiring_e2e.rs::plain_class_is_byte_identical_to_kotlinc` (compiles both via
CLI + kotlinc, asserts raw byte-equality; skips if kotlinc unavailable). Gates green: 2028 e2e, 29
conformance (box corpus), 8 metadata byte-exact, clippy clean.

How it was closed:
- `ClassWriter::seed_plain_class_pool` pre-interns the pool in kotlinc/ASM first-use order (ctor
  name+desc at entry; super() refs; per-field name/desc/fieldref; ctor LVT `this`/`Ldemo/C;`; accessor
  name+desc), so natural emission reuses them (intern dedups). Called from ir_emit gated exactly like
  the debug tables, BEFORE any add_field/add_method. Fixed divergences A/B/C.
- (D) `set_kotlin_metadata` interns each element KEY inline before its VALUEs (was: all keys up front).
- (E) finish() interns the SourceFile VALUE first (before Code) and the `SourceFile` name before
  `RuntimeVisibleAnnotations` (RVA last). This is a DEFAULT-PATH change — verified safe by the full
  suite (parity uses `javap -c`, ignoring pool order; box corpus is runtime).

Two earlier milestones this session:
1. **Data-class `@Metadata` byte-identical** to kotlinc. `is_data` added to `IrClass` (from AST, via
   ir_lower); `build_class_metadata` (src/jvm/ir_emit.rs) emits kotlinc's synthesized set
   (componentN/copy/equals/hashCode/toString + IS_DATA flag), all derived from `c.fields`.
2. **Debug tables** (LineNumberTable + LocalVariableTable) for a plain property class's synthesized
   members. `class C(val x: Int)` is now **byte-identical to kotlinc in SIZE (603B=603B) and pool
   count (36=36) and CONTENT — only constant-pool INTERNING ORDER still differs** (`cmp` diverges at
   char 47, inside the pool).

Supporting mechanics:
- `decl_line` on `ast::ClassDecl`, filled by parser post-pass `fill_class_decl_lines` (uses the `src`
  the Parser already holds — deliberately avoided threading through `lower_file`'s 27 call sites) →
  propagated to `IrClass.decl_line`.
- `src/jvm/classfile.rs`: `MethodInfo` gains `lnt`/`lvt`; `set_method_debug(name,desc,decl_line,locals)`;
  finish() writes LineNumberTable + LocalVariableTable Code sub-attrs (order LNT, LVT, then
  StackMapTable), attr-names interned conditionally. **Also fixed a latent default-path bug**:
  `StackMapTable` Utf8 was interned unconditionally (spurious pool entry in every branch-free class).
- `src/jvm/ir_emit.rs`: `attach_synth_debug_tables(c, cw)` — LNT `{0→decl_line}` + LVT (this + ctor
  params / this for getters / this+value for setters). Gated `computed.is_some() && !c.is_data`
  (opt-in with metadata, non-data only), called BEFORE the `@Metadata` attach.

Metadata + debug tables are OPT-IN (default off; `KRUSTY_EMIT_CLASS_METADATA=1` on CLI, or the test
helper `compile_in_process_with_class_metadata`). Default emit path unchanged → box corpus + parity safe.

## FIRST: commit this work

```
git add -A && git commit -F - <<'EOF'
feat(metadata): byte-identical class C(val x: Int) — @Metadata + debug tables + pool order

class C(val x: Int) compiled by krusty (class metadata on) is now BYTE-FOR-BYTE identical to
kotlinc 2.4.0 — the first fully byte-identical class. Pinned by a new e2e guard that compiles both
ways and asserts raw byte-equality.

- Data-class @Metadata: is_data on IrClass (from the AST via ir_lower); build_class_metadata emits
  kotlinc's synthesized componentN/copy/equals/hashCode/toString + IS_DATA flag.
- Debug tables: decl_line on ClassDecl (parser post-pass reusing the parser's src — no lower_file
  churn) -> IrClass; classfile.rs LineNumberTable/LocalVariableTable Code sub-attributes +
  set_method_debug; ir_emit attaches them to a plain property class's synthesized ctor+accessors.
- Constant-pool interning order matched to kotlinc/ASM: ClassWriter::seed_plain_class_pool
  pre-interns in first-use order; set_kotlin_metadata interleaves each key with its values; finish()
  interns the SourceFile value first and its name before RuntimeVisibleAnnotations.
- Also fixes a latent bug: StackMapTable was interned unconditionally, adding a spurious pool entry
  to every branch-free class.

@Metadata + debug tables + seeding stay opt-in/off by default; the finish() interning reorder is on
the default path but pool-order-only (parity uses javap -c). Tests: 2030 e2e, 29 conformance (box
corpus green), 8 class_builder byte-exact, clippy clean.
EOF
```

### Coverage note
The new debug-table/seeding code's branch coverage is exercised by three in-process guards in
`tests/data_class_metadata_wiring_e2e.rs`: `plain_class_emits_synth_debug_tables` (val/getter path),
`var_and_wide_slot_class_emits_debug_tables` (setter path + `Long`/`Double` `slot_size` branch), and
`data_class_metadata_wired_from_ir` (data-class metadata). Measured (lib+e2e, per-file): classfile.rs
95.05% lines, ir_emit.rs 88.80%, parser.rs 90.80%, ir_lower.rs 89.65% — all at/above the master
baseline (lines 87.25%, regions 85.99%, functions 90.15%, branches 73.01% in coverage-baseline.json),
so `scripts/coverage-gate.sh` should pass. LOCAL GOTCHA: coverage.sh calls `cargo +nightly llvm-cov`,
but here `~/.cargo/bin/cargo` is a real cargo (not the rustup proxy), so `+nightly` errors — run
coverage via `rustup run nightly cargo llvm-cov …` instead, or fix the cargo proxy. CI is unaffected.

## NEXT TASK: broaden byte-identity beyond `class C(val x: Int)`

The pool-order machinery now works for a single-`val` plain class. Broaden it, verifying each with the
byte-identity test pattern (compile both ways, `cmp`). Order of increasing difficulty:
1. **Multi-property + `var` + reference types**: `class C(val x: Int, var y: String)` — kotlinc 1236B.
   PROGRESS: `@NotNull`/`@Nullable` emission IMPLEMENTED (krusty 1058B → 1205B, gap now 31B). Two
   pre-existing gaps remain for full byte-identity (both separate from the annotation feature):
   - **(a) member setter missing null-check**: krusty's `setY(String)` body is `aload_0;aload_1;putfield;return`
     but kotlinc guards a non-null reference setter param with `checkNotNullParameter(value, "<set-?>")`
     first (the facade setter at ir_emit.rs ~1228 and the ctor at ~1503 DO this; the class-MEMBER
     accessor path does not). This is why krusty's pool lacks the `<set-?>` Utf8 + its String constant.
     The member getY/setY bodies come from the IR function emitter (they're IrFunctions in c.methods),
     so the fix is where the synthesized member accessor IR/body is built (lowering) — add the
     checkNotNullParameter prologue for a non-null reference setter param. (Also a minor SEMANTIC gap:
     krusty's setter accepts null where kotlinc throws.)
   - **(b) pool seeding not extended**: `seed_plain_class_pool` only seeds the single-property shape;
     the var+String pool (56 entries) needs the annotation strings (`Lorg/jetbrains/annotations/NotNull;`),
     the second property's field refs, getY/setY, `<set-?>`, and the `RuntimeInvisibleAnnotations`/
     `RuntimeInvisibleParameterAnnotations` attr names in kotlinc's first-use order (dump with
     `javap -v -p target/zz_var/kotlinc/demo/C.class`).
   ---
   IMPLEMENTED THIS SESSION (@NotNull, opt-in, all gates green): `ClassWriter.set_method_nullability` +
   MethodInfo `invisible_anns`/`param_anns` + finish() serialization (Code, then method
   RuntimeInvisibleAnnotations, then RuntimeInvisibleParameterAnnotations) + `attach_synth_nullability`
   in ir_emit (non-null ref return→@NotNull, ref params→@NotNull, nullable→@Nullable). The MEASURED
   kotlinc structure it reproduces:
   - `getY(): String` → method-level `RuntimeInvisibleAnnotations` = `@org.jetbrains.annotations.NotNull`
     on the RETURN.
   - `<init>(int, String)` → `RuntimeInvisibleParameterAnnotations` with `@NotNull` on param index 1
     (the String; the `int` param 0 gets an empty entry).
   - `setY(String)` → `RuntimeInvisibleParameterAnnotations` with `@NotNull` on its String param.
   - Primitive params/returns (`x: Int`, `getX`) get nothing.
   Rule: a non-null reference type (a `Ty` that is a reference and NOT `Ty::Nullable`) → `@NotNull`;
   a nullable reference → `@Nullable` (`Lorg/jetbrains/annotations/Nullable;`).
   IMPLEMENTATION NEEDED (writer has field-level `invisible_anns` but NO method-level or parameter
   annotations):
   - `MethodInfo`: add `method_invisible_anns: Vec<Vec<u8>>` (method RIA) and `param_annotations`
     (per-param annotation lists, for `RuntimeInvisibleParameterAnnotations`) + a `set_method_annotations`
     setter (mirror `set_method_debug`).
   - finish(): serialize both as method attributes AFTER `Code` (kotlinc order: Code, then
     RuntimeInvisibleAnnotations (method), then RuntimeInvisibleParameterAnnotations). Intern the attr
     names conditionally.
   - Emit for the synth members in `attach_synth_debug_tables` (or a sibling): getter return + ctor/setter
     ref params.
   - Extend `seed_plain_class_pool` for the new pool entries in kotlinc order (from the kotlinc pool:
     `Lorg/jetbrains/annotations/NotNull;`, `RuntimeInvisibleAnnotations` (#50), `LineNumberTable` (#52),
     `LocalVariableTable` (#53), `RuntimeInvisibleParameterAnnotations` (#54), `RuntimeVisibleAnnotations`
     (#56) — dump the full pool with `javap -v -p target/zz_var/kotlinc/demo/C.class` and read off order).
   NOTE: the "Signature 4" in a raw javap attr-count grep for this class is a FALSE match (the LVT
   "Name Signature" column header) — this class has no generics, so no real Signature attribute.
   Verify with the byte-identity test pattern (compile both, `cmp`).
2. **Data classes**: extend `attach_synth_debug_tables` + seeding to the synthesized data methods
   (component/copy/equals/hashCode/toString). These have branches → StackMapTable, so verify the Code
   sub-attribute order (LNT, LVT, StackMapTable) and the per-method line numbers vs kotlinc.
3. **Real user-method bodies**: the large remaining piece — per-instruction LineNumberTable needs
   source-position threading through codegen (IR carries none today), and LocalVariableTable needs
   local-scope range tracking. This is the multi-session frontend+backend effort.

The seeding approach (pre-intern in kotlinc order, let natural emission reuse) generalizes: for each
new shape, dump kotlinc's pool with `javap -v -p`, read off the first-use order, and extend
`seed_plain_class_pool` (or add per-shape seeders) to match. `set_kotlin_metadata`/`finish()` order is
already aligned.

### Reference: the interning-order rules (already applied for the plain class)

kotlinc order (verified for `class C(val x:Int)`), 1-based pool:
```
1 demo/C  2 Class#1  3 java/lang/Object  4 Class#3
5 <init>  6 (I)V  7 ()V  8 NT<init>:()V  9 Methodref Object.<init>
10 x  11 I  12 NT x:I  13 Fieldref C.x
14 this  15 Ldemo/C;
16 getX  17 ()I
18 Lkotlin/Metadata; 19 mv 20 Int2 21 Int4 22 Int0 23 k 24 Int1 25 xi 26 Int48 27 d1 28 <bytes> 29 d2 30 ""
31 C.kt  32 Code  33 LineNumberTable  34 LocalVariableTable  35 SourceFile  36 RuntimeVisibleAnnotations
```
The rule: header → per-method [name_utf8, desc_utf8 AT ENTRY, body refs in bytecode order, then LVT
name/desc utf8s] → fields reuse (interned lazily, NOT at add_field) → @Metadata (type; mv; then the
Integers INTERLEAVED with k/xi/d1/d2 as element pairs are visited) → SourceFile value + attribute-name
utf8s last.

Five concrete krusty divergences to fix:
- (A) `add_field*` interns backing-field name/desc (x, I) at field-decl time; kotlinc interns them
  lazily at the putfield in `<init>`. Fix: defer field name/desc interning to finish()'s field-table
  write (utf8() dedups, so if methods emitted first, indices are reused).
- (B) method descriptor (`(I)V`) is interned late (add_method_sig runs after the CodeBuilder is built);
  kotlinc interns method name+desc AT METHOD ENTRY, before the body. Fix: intern name+desc before
  building each method's code.
- (C) `attach_synth_debug_tables` batches all methods' LVT strings after every method is added; kotlinc
  interns each method's LVT strings right after that method's body, before the next method. Fix:
  interleave — attach a method's debug table immediately after it's added.
- (D) `set_kotlin_metadata` interns all string keys before the Integer values; kotlinc interleaves
  Integers between mv/k/xi/d1 as it visits each annotation element.
- (E) finish() trailing order: kotlinc = C.kt(value), Code, LineNumberTable, LocalVariableTable,
  SourceFile, RuntimeVisibleAnnotations. krusty differs. Align the finish() interning order.

Because (A)+(B) restructure the core add_field/add_method interning for EVERY class, do them behind the
`javap -c` safety net and run the full suite after each. Alternative if the emission restructure proves
too invasive: a post-hoc constant-pool permutation pass in finish() that reorders to the canonical
order above and rewrites every u2 index — bigger but localized to classfile.rs.

## Verify loop
```
export JAVA_HOME=/Users/qnox/Library/Java/JavaVirtualMachines/jbr-21.0.9/Contents/Home
rustup run nightly cargo build --profile gate --bin krusty
D=target/zz_cval; rm -rf $D; mkdir -p $D/kotlinc $D/krusty
printf 'package demo\nclass C(val x: Int)\n' > $D/C.kt
target/cache/kotlinc/2.4.0/kotlinc/bin/kotlinc $D/C.kt -d $D/kotlinc
KRUSTY_EMIT_CLASS_METADATA=1 ./target/gate/krusty -d $D/krusty $D/C.kt
cmp $D/kotlinc/demo/C.class $D/krusty/demo/C.class && echo "BYTE-IDENTICAL"
# regression gates:
rustup run nightly cargo test --profile gate --test e2e
rustup run nightly cargo test --profile gate --test conformance   # box corpus MUST stay green
just clippy-baseline-check
```
Run the test SUITE via the JAVA_HOME'd cargo above (or run-tests.sh/just) — bare `cargo test` skips the
box corpus. SSH is dead; push via `TOK=$(gh auth token); git -c credential.helper='!f(){ echo
username=x-access-token; echo password='"$TOK"'; }; f' push https://github.com/qnox/krusty.git HEAD`.

After `class C` is fully byte-identical: extend debug tables to the data-class synthesized methods
(they have branches → StackMapTable; verify Code sub-attr order with a stackmap), then broaden to
real user-method bodies (needs per-instruction line numbers → source-position threading through
codegen, the large remaining frontend+backend piece).
```
