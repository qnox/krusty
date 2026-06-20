# Differential testing vs the real kotlinc

`tests/diff_kotlinc.rs` / `tests/diff_class_kotlinc.rs` compile the same source with **krusty** and the
real **kotlinc**, then assert the public ABI (javap signatures) and execution output match. Gated on
env vars; skipped if unset.

## Reference kotlinc from local jars (no assembled dist)

No `kotlinc/lib` dist was available, so build a launcher from cached jars. kotlinc 2.0.21 needs **JDK
≤ 21** (it rejects JDK 25's version string). Classpath: `kotlin-compiler-embeddable` + `kotlin-stdlib`
+ `kotlin-reflect` + `kotlin-script-runtime` + `kotlinx-coroutines-core-jvm` + `trove4j` +
`org.jetbrains:annotations`. Pass `-classpath <stdlib>` so compilation sees the stdlib API.

```sh
#!/bin/sh
exec <JDK21>/bin/java -cp "<all jars above, ':'-joined>" \
  org.jetbrains.kotlin.cli.jvm.K2JVMCompiler -classpath "<kotlin-stdlib.jar>" "$@"
```

## Run

```sh
export JAVA_HOME=<modern JDK to run javac/java for krusty output>
export KRUSTY_REF_JAVA_HOME=<JDK21>            # runs kotlinc
export KRUSTY_KOTLINC=/path/to/kotlinc-wrap.sh
export KRUSTY_KOTLIN_STDLIB=<kotlin-stdlib.jar>  # on the runtime cp for kotlinc output
cargo test --test diff_kotlinc --test diff_class_kotlinc -- --nocapture
```

Result (this session): both pass — krusty ABI + execution match kotlinc on the supported subset.

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
