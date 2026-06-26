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
