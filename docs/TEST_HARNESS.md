# Test Harness

Use `./run-tests.sh` as the canonical test entrypoint. It is self-provisioning and normally needs no
parameters.

## Agent Quick Reference

- Use `./run-tests.sh` for the full suite; it provisions kotlinc and the Kotlin codegen/box corpus.
- Use focused harness runs, not raw `cargo test`, while iterating: `./run-tests.sh --test <name> -- --nocapture`.
- Do not pass `--release`; the gate profile is the intended fast edit/build/test loop.
- For conformance changes, run `./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture` and keep `FAIL: 0`.
- For performance work, start with the harness timing output or `KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1`.

## Normal Runs

```sh
./run-tests.sh
```

`just test` is equivalent. When `just` is available, the harness provisions the matching Kotlin
compiler and codegen/box corpus, exports `KRUSTY_KOTLINC` and `KRUSTY_KOTLIN_BOX_DIR`, builds the test
binaries once with Cargo's `gate` profile, runs the internally parallel conformance binary alone, then
runs the remaining test binaries in slow-first parallel order.

Do not use `--release` for tests. The release build cycle takes longer than it saves at runtime, and
`run-tests.sh --release` is rejected intentionally.

## Focused Runs

Pass normal Cargo test arguments through the harness:

```sh
./run-tests.sh --test metadata_return_types
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Any argument switches the harness to Cargo's normal focused runner with `--profile gate`. This is
useful for development, but use the no-argument harness for full-suite validation because it builds
once and schedules test binaries to preserve shared JVM runners.

## Profiling

For full-suite performance work, run:

```sh
./run-tests.sh
```

The final `SLOWEST TEST BINARIES` table is the first profiling signal. Use it before changing tests
or inventing custom loops.

For compiler-only conformance profiling, use:

```sh
KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1 ./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

This skips JVM execution in the conformance test, prints phase timing, and writes
`target/flamegraph.svg`.

Optional profiling knobs:

- `KRUSTY_TEST_JOBS=<n>` overrides full-suite test-binary parallelism.
- `KRUSTY_TEST_THREADS=<n>` overrides conformance worker threads.
- `KRUSTY_BOX_LIMIT=<n>` caps conformance corpus scanning for fast sampling.
- `KRUSTY_FAIL_CAP=<n>` caps reported conformance failures.

Optional compiler trace:

- `KRUSTY_TRACE=resolve` prints selected classpath call-resolution decisions.
- `KRUSTY_TRACE=all` enables every compiler trace category.
- `KRUSTY_IR_DEBUG=1` prints the lowerer bail reason when the JVM backend skips a file.

Trace output is disabled by default, reads the environment once, and does not format trace messages
unless the requested category is enabled.

## Current Conformance

Latest verified codegen/box metric (2026-06-28):

```text
scanned: 7351 | krusty-compiled: 2078 | box()=OK: 2078 | skipped(unsupported): 5273 | FAIL: 0
```

Only compare `box()=OK` numbers when `FAIL: 0`. The historical `1842 -> 1585` cliff in
`target/ir_conformance_trend.csv` was a real temporary coverage drop from a conformance-safety cleanup,
not the current metric. That cleanup stopped counting unsupported shapes as compiled support
(builder-inference directives, JS-runtime-only files, advanced `Result<T>`/value-class cases, and
unsupported `UByte`/`UShort` value-class paths). Later passes recovered past both plateaus; this checkout
is currently at `2078 OK / 0 FAIL`. Likewise, `KRUSTY_NO_RUN=1` is for compile/emit profiling only; it
skips JVM execution and must not be reported as runtime conformance.

For corpus triage, use the survey binary through the gate profile:

```sh
cargo run --profile gate --bin survey -- target/cache/box-corpus/2.4.0/compiler/testData/codegen/box
cargo run --profile gate --bin survey -- target/cache/box-corpus/2.4.0/compiler/testData/codegen/box --samples "inline splice failed"
```

The survey reuses the same provisioned toolchain/cache paths as the harness and reports specific
inline splice bail callees when available.

## JVM-Running Tests

Do not spawn `javac` or `java` per test unless the test is explicitly about the CLI/process boundary.
Use the shared helpers in `tests/common`:

- `compile_and_run_box`
- `run_box`
- `javac_run`

These helpers compile in process where possible and reuse persistent JVM runners/servers inside a test
binary. Per-test JVM startup is one of the easiest ways to degrade the suite.

## Environment Overrides

The harness usually sets these itself through `just`. Override them only when testing a specific local
toolchain:

```sh
KRUSTY_KOTLINC=/path/to/kotlinc/bin/kotlinc \
KRUSTY_REF_JAVA_HOME=/path/to/jdk \
KRUSTY_KOTLIN_BOX_DIR=/path/to/compiler/testData/codegen/box \
KRUSTY_KOTLIN_STDLIB=/path/to/kotlin-stdlib.jar \
./run-tests.sh
```
