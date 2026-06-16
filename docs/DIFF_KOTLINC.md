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
