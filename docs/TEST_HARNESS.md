# Test Harness

Use `./run-tests.sh` as the canonical test entrypoint. It is self-provisioning and normally needs no
parameters.

## Agent Quick Reference

- Use `./run-tests.sh` for the full suite; it provisions kotlinc and the Kotlin codegen/box corpus.
- Use focused harness runs, not raw `cargo test`, while iterating. Standalone suites still use `./run-tests.sh --test <name> -- --nocapture`; grouped e2e tests use a test-name filter, e.g. `./run-tests.sh --test e2e lambda_e2e::lambdas_run -- --nocapture`.
- Do not pass `--release`; the gate profile is the intended fast edit/build/test loop.
- For Kotlin box conformance changes, run `./run-tests.sh --test conformance kotlin_codegen_box_conformance -- --nocapture` and keep `FAIL: 0`.
- For performance work, start with the harness timing output or `KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1`.

## Normal Runs

```sh
./run-tests.sh
```

The LSP crate also has an opt-in protocol differential against JetBrains' official Kotlin LSP. It
compares normalized diagnostic ranges, severity, source, and messages, plus decoded semantic-token
types and modifiers, rather than raw protocol token indexes whose legends can differ. Point the
environment variable at an installed official launcher; the regular suite does not download the
roughly 400 MB, platform-specific distribution. The differential creates a minimal Gradle project
using the highest version in `kotlin-versions`, because current official servers do not analyze loose
source files without a workspace model:

```sh
KRUSTY_KOTLIN_LSP=/path/to/bin/intellij-server \
./run-tests.sh -p krusty-lsp --test kotlin_lsp_diff -- --nocapture
```

The compiler diagnostic differential uses the provisioned kotlinc and compares each first error's
source filename, 1-based line and column, and exact message. A matching message at the wrong call,
argument, member, initializer, or assignment location is a test failure.

`just test` is equivalent. When `just` is available, the harness provisions the matching Kotlin
compiler and codegen/box corpus, exports `KRUSTY_KOTLINC` and `KRUSTY_KOTLIN_BOX_DIR`, builds the test
binaries once with Cargo's `gate` profile, runs the internally parallel conformance binary alone, then
runs the remaining test binaries in slow-first parallel order.

Do not use `--release` for tests. The release build cycle takes longer than it saves at runtime, and
`run-tests.sh --release` is rejected intentionally.

## Focused Runs

Pass normal Cargo test arguments through the harness:

```sh
./run-tests.sh --test conformance -- --nocapture
./run-tests.sh --test e2e lambda_e2e::lambdas_run -- --nocapture
```

Product e2e files are grouped into one `e2e` integration-test binary; external corpus/reference-toolchain suites are grouped into a separate `conformance` binary. Cargo compiles each
top-level `tests/*.rs` file as a separate crate, so grouping keeps link count and build artifacts
bounded. Focus a grouped test with a module/test-name filter (`lambda_e2e::lambdas_run`, a test function
name, or any normal libtest substring). The conformance suite remains available by `--test conformance` and is excluded from fast/coverage runs before it executes.

Any argument switches the harness to Cargo's normal focused runner with `--profile gate`. This is
useful for development, but use the no-argument harness for full-suite validation because it builds
once and schedules test binaries to preserve shared JVM runners.

## Byte-Identity Differential Mode

`KRUSTY_BYTE_DIFF=1` makes the box-conformance run ALSO compile every krusty-compiled corpus file
with the reference kotlinc (persistent in-process compiler server; results cached under
`target/cache/ref-classes/`, keyed by source + stem + classpath + dist identity) and compare the
two class sets **byte-for-byte**:

```sh
KRUSTY_BYTE_DIFF=1 KRUSTY_SERVER_POOL=4 ./run-tests.sh --test conformance -- --nocapture
```

The summary gains a `byte-diff: identical I | divergent D | ref-fail R | not-diffed N` line, and a
per-file report (first difference per file) lands in `target/byte_diff_report.txt`. `// MODULE:`
and mixed-Java tests are `not-diffed` (their reference orchestration isn't mirrored yet), and
kotlinc's `META-INF/*.kotlin_module` artifact is not yet compared. The first run pays one warm
kotlinc compile (~0.4 s) per file — raise `KRUSTY_SERVER_POOL` on a large-RAM host; later runs hit
the on-disk cache. Pair with `KRUSTY_BOX_ONLY=<substring>` for a focused divergence loop.

## Profiling

For full-suite performance work, run:

```sh
./run-tests.sh
```

The final `SLOWEST TEST BINARIES` table is the first profiling signal. Use it before changing tests
or inventing custom loops.

For compiler-only conformance profiling, use:

```sh
KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1 ./run-tests.sh --test conformance kotlin_codegen_box_conformance -- --nocapture
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
- `KRUSTY_TRACE=lower` prints IR lowerer bail reasons when the JVM backend skips a file.
- `KRUSTY_TRACE=all` enables every compiler trace category.

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
inline splice bail callees when available. It covers the full corpus shape set the gate compiles:
single-file, `// FILE:`-split multi-file (with the generated `// WITH_COROUTINES` helpers), and
`// MODULE:` multi-module tests (each build unit compiled against its dependency modules' emitted
classes, `dependsOn` chains folded in — the splitting lives in `krusty::conformance`, shared with
the gate). Tests with `.java` sources are the one exception: they need the harness's persistent
javac runner, so the survey reports them under a dedicated `javac-dependent` category instead of a
first compiler error.

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
