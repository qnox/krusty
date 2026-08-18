# Differential testing vs the real kotlinc

The differential oracle is the **box-corpus conformance gate** (`tests/kotlin_box_ir_jvm_conformance.rs`):
it compiles each corpus source with **krusty** and runs `box()` on the JVM, grading against the real
**kotlinc**'s expectations. The reference kotlinc and the box corpus are **self-provisioned** (downloaded
+ cached under `target/cache/`) at the version pinned by the `kotlin-versions` manifest — currently
**2.4.0**. No assembled dist or env wrangling is needed.

## Run

```sh
just conformance          # prints "<pct> <passed> <scanned>"; self-provisions kotlinc + corpus
just test                 # full suite (the gate + all e2e), the pre-push GATE
./harness/run-diff.sh     # thin wrapper around `just conformance`
```

`KRUSTY_KOTLINC` / `KRUSTY_KOTLIN_BOX_DIR` are honored if already exported, else `just` fills them in
from `just kotlinc` / `just box-corpus`. A modern `JAVA_HOME` (≥ 21) runs krusty's output and `javap`.

## Normalized bytecode diff (`src/bin/bytediff.rs`)

The `box()=OK` conformance gate proves *runtime* correctness; this tool measures the project's harder
goal — emitting the **same bytecode** kotlinc does. For each box-corpus file that BOTH compilers accept,
it compares per-class disassembly (`javap -c -p`) after normalizing away what differs without changing
semantics: the source-file banner, per-instruction bytecode offsets, and constant-pool index tokens
(`#21`). Two classes that normalize equal have identical method signatures and identical instruction
sequences (the resolved `// Method …` / `// String …` operands are kept).

Opt-in and slow (one kotlinc JVM launch per file) — NOT part of the <60s test gate. Run on a sample:

```sh
export JAVA_HOME=<jdk>                                   # runs javap
export KRUSTY_KOTLINC=$(just kotlinc "$(just max-version)")
export KRUSTY_SURVEY_STDLIB=<kotlinc dist>/lib/kotlin-stdlib.jar
export KRUSTY_SURVEY_JDK_MODULES=$JAVA_HOME/lib/modules
cargo run --release --bin bytediff -- "$(just box-corpus "$(just max-version)")" 200 [--samples]
```

It prints `files compiled by both`, `classes compared`, `byte-identical (normalized)` + %, and
`krusty-only classes` (a class krusty emits that kotlinc doesn't — a structural divergence). `--samples`
prints the first diverging normalized line per differing class, to localize where codegen drifts. This is
the instrument that drives the bytecode-equality goal: pick a divergence, fix the emitter, re-measure.

## Measured divergences (2026-08-18)

Baseline from the command above over the first 60 corpus files: **89 of 177 classes byte-identical
(50.3%)**, 8 krusty-only classes. Each entry below is measured against kotlinc 2.4.10 — the shape is
what the reference actually emits, not what it plausibly emits — so the next person can start from the
fix rather than from the measurement.

### A `KClass` annotation member is `java.lang.Class` in the class file

The largest identified cluster. `KClass<*>` is not a legal JVM annotation element type, so an
annotation declaring one cannot be read reflectively at all. kotlinc's shape is a CONVERSION, and it
spans three places that must change together:

| where | kotlinc |
| --- | --- |
| annotation interface | `()Ljava/lang/Class;` + `Signature: ()Ljava/lang/Class<*>;` |
| implementation ctor | parameter stays `KClass<?>`; `JvmClassMappingKt.getJavaClass` before `putfield` |
| implementation field/accessor | `Class` |
| implementation `equals` | both sides through `Reflection.getOrCreateKotlinClass`, compared as `KClass` |
| every READ SITE | source sees `KClass<*>`, the getter returns `Class` — the read wraps in `Reflection.getOrCreateKotlinClass` |

That last row is the trap: PR #689 changed only the interface descriptor, left the implementation
returning `KClass`, and produced two class files that compiled, passed every descriptor and metadata
assertion and the whole conformance corpus, then threw `NoSuchMethodError` on first use. Reverted in
#690. **Any change here needs a test that RUNS the program**, not one that inspects descriptors — the
corpus does not cover it, because no sampled file both declares a `KClass` member and instantiates the
annotation.

### An inlined call's arguments are spilled to locals before the body

kotlinc evaluates each argument of an inlined function into a local and reads it back inside the
inlined body; krusty evaluates in place. `println(f())`:

```
kotlinc   invokestatic f()I; istore_0; getstatic System.out; iload_0; invokevirtual println(I)V
krusty    getstatic System.out; invokestatic f()I; invokevirtual println(I)V
```

Same result; different local-slot allocation, which is why `astore_`/`aload_` rows dominate what is
left of the sample. A user `inline fun` shows it too, plus an inline-marker local kotlinc allocates
(`iconst_0; istore_1`) and krusty does not. This is the broadest remaining divergence and touches the
inliner rather than an emitter.

### Smaller, self-contained

- **Annotation implementation attributes.** All five methods and the access word match after #691–#695,
  but krusty emits no `LocalVariableTable` (kotlinc names `this`/`other`) and no `@Nullable` on
  `equals`'s parameter, so whole-file `cmp` still differs.
- **`Int::class` as an annotation argument** encodes `Ljava/lang/Integer;`; kotlinc writes the primitive
  `I` (`Boolean::class` → `Z`). Not an emitter bug: `class_literal_unbound_ty` deliberately models a
  primitive class literal as its boxed wrapper so it compares equal to a bound literal (`42::class`),
  and the comment there predicts this. Correcting it changes what `Int::class` denotes in the checker.
- **Class-literal forms rejected as annotation arguments**: `Unit::class`, `UInt::class`,
  `IntArray::class`, `Array<String>::class` all fail to compile; kotlinc accepts all four, encoding
  `Lkotlin/Unit;`, `Lkotlin/UInt;`, `[I`, `[Ljava/lang/String;`.
