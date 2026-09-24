# Test Harness

Use `./run-tests.sh` as the canonical test entrypoint. It is self-provisioning and normally needs no
parameters.

## Agent Quick Reference

- Use `./run-tests.sh` for the full suite; it provisions kotlinc and the Kotlin codegen/box corpus.
- **Without `just`, provision kotlinc by hand — do not proceed without it.** `run-tests.sh` reaches
  the reference compiler through `just kotlinc <ver>`, so in an environment that has no `just` every
  differential test fails with `reference compiler unavailable` and a run of them proves nothing. The
  recipe is a plain download, so do what it does:

  ```sh
  ver=2.4.20
  dest="$PWD/target/cache/kotlinc/$ver"
  mkdir -p "$dest"
  curl -fsSL "https://github.com/JetBrains/kotlin/releases/download/v${ver}/kotlin-compiler-${ver}.zip" -o /tmp/kotlinc.zip
  python3 -c "import zipfile; zipfile.ZipFile('/tmp/kotlinc.zip').extractall('$dest')"
  chmod +x "$dest/kotlinc/bin/"*
  export KRUSTY_KOTLINC="$dest/kotlinc/bin/kotlinc"
  ```

  `KRUSTY_KOTLINC` is what the harness reads, so exporting it is the whole of the setup. This matters
  more than it looks: kotlinc is the correctness oracle (`docs/SPEC.md` §6), so a change validated
  without it has been checked against an expectation rather than against the reference — which is
  exactly the mistake the differential harness exists to prevent.
- Use focused harness runs, not raw `cargo test`, while iterating. Standalone suites still use `./run-tests.sh --test <name> -- --nocapture`; grouped e2e tests use a test-name filter, e.g. `./run-tests.sh --test e2e lambda_e2e::lambdas_run -- --nocapture`.
- Use `./run-tests.sh --survey --parse-only --report /tmp/krusty-parse.tsv` for the syntax-only
  conformance gate. It parses every Kotlin block independently, ignores Java fixture blocks, checks
  AST integrity, and never enters signature collection or checking.
- Use `./run-tests.sh --survey --frontend-only` to audit parser/signature/checker skips against the
  pinned corpus without building or running the backend. This is deliberately not a parser metric.
- Do not pass `--release`; the gate profile is the intended fast edit/build/test loop.
- `gate` is a Cargo *profile* (`--profile gate`), never a target directory. `--target-dir target/gate`
  or `CARGO_TARGET_DIR=target/gate` silently builds the *dev* profile into `target/gate/debug`; the
  harness refuses that shape. It also prunes orphan `*.rcgu.o` codegen temporaries (left by any rustc
  killed mid-build) older than six hours before building — cargo never garbage-collects `target/`
  itself, so stale profile/feature variants still need an occasional
  `cargo clean -p krusty -p krusty-cli -p krusty-lsp --profile gate` (drops only workspace crates,
  keeps third-party deps).
- For Kotlin box conformance changes, run `./run-tests.sh --test conformance kotlin_codegen_box_conformance -- --nocapture` and keep `FAIL: 0`.
- For performance work, start with the harness timing output or `KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1`.

## Normal Runs

```sh
./run-tests.sh
```

The LSP crate also has an opt-in protocol differential against JetBrains' official Kotlin LSP. It
compares normalized diagnostic ranges, severity, source, and messages, decoded semantic-token types
and modifiers, exact definition and type-definition target URIs/ranges, complete sorted transitive
implementation locations, find-reference declaration filtering and location sets, complete hover
markdown/ranges, and stable completion labels, kinds, label details, ranking, and incomplete status.
Every navigation comparison checks both UTF-16 endpoints; matching text at the wrong location fails.
Implementation coverage includes class and member declarations, references, generic substitution,
overload selection, `null` leaf results, and a query following a supplementary-plane character.
Rename compares the complete `WorkspaceEdit`, including document URI/version, edit ordering,
replacement text, and both UTF-16 range endpoints for cross-file, lexical, overload-selected,
Unicode-offset, and backticked identifiers.
Hierarchical document symbols compare the complete protocol value, including order, nesting, names,
kinds, deprecation/tags, full ranges, and selection ranges. A correct symbol at the wrong location
therefore fails just like a diagnostic at the wrong location.
The same differential requires incremental synchronization capability and applies ordered ranged
edits before comparing the resulting definition URI and exact UTF-16 range.
The test does not compare raw protocol token indexes whose legends can differ or implementation
specific completion commands and opaque data. Point the environment variable at an installed
official launcher; the regular suite does not download the roughly 400 MB, platform-specific
distribution. The differential creates a minimal Gradle project using the highest version in
`kotlin-versions`, because current official servers do not analyze loose source files without a
workspace model:

```sh
KRUSTY_KOTLIN_LSP=/path/to/bin/intellij-server \
./run-tests.sh -p krusty-lsp --test kotlin_lsp_diff -- --nocapture
```

The compiler diagnostic differential uses the provisioned kotlinc and compares each first error's
source filename, 1-based line and column, and exact message. A matching message at the wrong call,
argument, member, initializer, or assignment location is a test failure.

`just test` is equivalent. When `just` is available, the harness provisions the matching Kotlin
compiler and codegen/box corpus, exports `KRUSTY_KOTLINC` and `KRUSTY_KOTLIN_BOX_DIR`, builds the test
binaries once with Cargo's `gate` profile, runs the conformance binary alone in two passes (box
corpus, then everything else), then runs twenty-two balanced whole-module shards of the internally
parallel e2e binary, then runs the remaining small test binaries in parallel. `KRUSTY_E2E_SHARDS`
overrides the shard count.

Each scheduled invocation owns its log. An unfiltered binary keeps the plain `<binary>.log` name; a
filtered invocation appends an `@<filter-slug>` derived by `run_label`, so the two conformance logs are
`conformance-<hash>@kotlin_codegen_box_conformance.log` and
`conformance-<hash>@skip-kotlin_codegen_box_conformance.log`. Scheduling-only
`--test-threads=<count>` arguments do not change identity. Because slugging is deliberately lossy,
`run_one` adds `#2`, `#3`, and so on if a derived name is already present instead of overwriting an
earlier run. The failure report reads the exact invocation's log, and the timing table lists each
invocation separately because each is a separate process with its own wall time. E2e shard logs use
the explicit labels `shard-1-of-22`, and so on; every shard's reported selected-test count must equal
the planner's count, so filtering cannot silently reduce coverage.

CI builds the conformance test binary once and runs that artifact against every version in
`kotlin-versions`. `KRUSTY_LANGUAGE_VERSION`, `KRUSTY_KOTLINC`, and `KRUSTY_KOTLIN_BOX_DIR` select the
runtime reference toolchain, so the matrix does not rebuild Rust code per Kotlin version. Each leg
uses the same configurable process-group conformance deadline as the local harness, including its
spawned compiler and runner JVMs. A release publishes only after every leg matches its expected-failure
list.

## Box Expected-Failure Lists

`tests/box_expected_failures/<version>.txt` names, one path per line relative to
`compiler/testData/codegen/box`, the corpus files krusty is still allowed to fail against that Kotlin
release. The conformance test fails when a file outside the list fails (a regression) or a file in the
list passes (a fix: delete the entry in the same change). A listed path that is absent from the corpus
or not applicable to the JVM backend fails as a stale entry. Sharded and filtered runs judge only the
files they ran. A version without a list must pass every applicable file.

After a change that fixes or deliberately trades box files, rewrite the list from a full run and commit
the diff with the change, so review sees exactly which files moved:

```sh
KRUSTY_BLESS_BOX_FAILURES=1 KRUSTY_LANGUAGE_VERSION=<v> ./run-tests.sh --test conformance kotlin_codegen_box_conformance -- --nocapture
```

Blessing is refused under CI and on a partial run. The goal is to empty every list; once they are
empty the lists and `tests/box_ratchet.rs` are deleted.

The rest of the suite is version-sensitive too, because the supported kotlinc releases do not word
every diagnostic alike (see `docs/SPEC.md` §6). `KRUSTY_LANGUAGE_VERSION=<v> ./run-tests.sh` runs the
whole suite with krusty reproducing release `<v>` against that release's kotlinc; `just test-all`
does it for every manifest version at once.

A test that pins what kotlinc reports does not write it down: it states what it observes and reads
the value from `tests/recorded/<test module>.txt`, keyed by the running test's path and by Kotlin
version range (`tests/common/recorded.rs`). `common::assert_errors_match_kotlinc` (the whole
`file:line:column: message` ledger through both CLIs) and `common::assert_messages_match_kotlinc`
(the frontend's messages) cover the usual shapes; `common::recorded(|| …)`,
`common::recorded_named(label, || …)` and `common::recorded_line(|| …)` take any value computed from
kotlinc's run. When the file has no value for the version under test, a local run computes it from
that kotlinc, writes it and passes; commit the diff. Ranges are closed and merged across adjacent
versions (`2.4.0..2.4.10:`), while the newest version remains explicit (`2.4.20:`). A newly supported
release therefore has no value until its own kotlinc records one. Under CI (`CI` set) a missing value
fails instead of recording.
`KRUSTY_RECORD=1` re-records every value the run reaches, e.g. after a kotlinc patch update:
`KRUSTY_RECORD=1 KRUSTY_LANGUAGE_VERSION=<v> ./run-tests.sh --test e2e -- <filter>`. Only kotlinc's
output is ever recorded, so a recorded value stays an oracle for krusty.

The general test-binary deadline defaults to 120 seconds. Each conformance pass defaults to 295
seconds and can be adjusted with `KRUSTY_CONFORMANCE_TIMEOUT_SECONDS`; each product e2e shard
defaults to 295 seconds and can be adjusted independently with `KRUSTY_E2E_TIMEOUT_SECONDS`.

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
`target/flamegraph.svg` plus a `top krusty frames` table on stderr. The whole run fits inside the
harness deadline (`KRUSTY_CONFORMANCE_TIMEOUT_SECONDS`, 295s): the harness symbolizes each sampled
instruction pointer once rather than once per stack, so turning ~80k samples into an SVG costs a
couple of seconds instead of the ~8 minutes `pprof`'s own `Report::build` takes on a full-corpus
profile. The SVG covers every sampled stack and runs to tens of megabytes — it is meant to be opened
in a browser, not read as text.

The e2e suite has its own built-in phase profiler: `KRUSTY_PROF=1` makes every harness helper print
`PROF\t<phase>\t<ms>` lines (`krusty` in-process compile, `kotlinc` reference compile incl. queue
wait, `box` JVM round-trip) to stderr — run the e2e binary with `--nocapture` and aggregate.

Performance-relevant harness state:

- e2e dependency libs are compiled BY KRUSTY, in-process (`tests/common::compile_libs`), memoized
  per run — no reference-compiler round-trip and deliberately no on-disk cache: every run rebuilds
  its deps with the compiler under test. A lib krusty can't build fails the test with krusty's
  diagnostics; tests whose CONTRACT is consuming kotlinc-emitted metadata declare it with the
  explicit `*_ref` helpers (`compile_lib_ref`, `run_box_against_ref`, `Fixture::reference_lib`) —
  grep `_ref(` for the current emission/consumption gap inventory.
- The dependency-lib differential is ON BY DEFAULT: every krusty-built lib is also compiled with
  the reference kotlinc and the same `box()` result is asserted against both classpaths. Disable
  explicitly with `KRUSTY_LIB_CROSSCHECK=0` for a fast local loop. The assertion is BEHAVIORAL
  (same `box()` result), not byte-identity: lib classfiles still diverge from kotlinc's bytes
  (constant-pool ordering, `.kotlin_module` emission). `KRUSTY_LIB_BYTEDIFF_REPORT=1` (with
  `--nocapture`) prints a `LIBDIFF\t<identical|divergent|krusty-only|kotlinc-only>\t<entry>` line
  per lib entry — the convergence inventory for making byte equality the assertion.
- Persistent JVM pools (kotlinc compiler servers, JavaRunner) scale with the host: `ncpu/2` clamped
  to `[1, 6]`. `KRUSTY_SERVER_POOL=<n>` overrides in either direction (e.g. `1` on a swapping host).
- Directory classpath entries are shipped into the box runner's per-request classloader, so lib
  static state is fresh per `box()` call and runner JVMs are shared across tests.

Optional profiling knobs:

- `KRUSTY_TEST_TIMEOUT_SECONDS=<seconds>` overrides the 120-second deadline applied to every test
  binary except conformance and e2e; raise it explicitly on slow systems.
- `KRUSTY_CONFORMANCE_TIMEOUT_SECONDS=<seconds>` overrides the 295-second deadline for each
  full-suite or focused conformance pass.
- `KRUSTY_E2E_TIMEOUT_SECONDS=<seconds>` overrides the 295-second deadline for focused e2e runs and
  each full-suite e2e shard.
- `KRUSTY_E2E_SHARDS=<count>` overrides the twenty-two whole-module shards used by the plain full-suite
  run.
- `KRUSTY_TEST_JOBS=<n>` overrides full-suite test-binary parallelism.
- `KRUSTY_TEST_THREADS=<n>` overrides conformance worker threads.
- `KRUSTY_BOX_LIMIT=<n>` caps conformance corpus scanning for fast sampling.
- `KRUSTY_FAIL_CAP=<n>` caps reported conformance failures.
- `KRUSTY_BLESS_BOX_FAILURES=1` rewrites the Kotlin version's box expected-failure list from a full
  conformance run instead of checking against it.

Optional compiler trace:

- `KRUSTY_TRACE=resolve` prints selected classpath call-resolution decisions.
- `KRUSTY_TRACE=lower` prints IR lowerer bail reasons when the JVM backend skips a file.
- `KRUSTY_TRACE=all` enables every compiler trace category.

Trace output is disabled by default, reads the environment once, and does not format trace messages
unless the requested category is enabled.

## Current Conformance

There is no conformance number written down in this repository, deliberately. Every figure committed
to a document went stale within days of the commit that changed it, and a stale number read as
current is worse than no number. The live measure is the **conformance badge** in `README.md`: the
`conformance` job recomputes it per reference version on every master build and publishes the share
of the `codegen/box` corpus whose `box()` returns `OK` on krusty-emitted bytecode. Read the badge, or
the `conformance` job of the latest master CI run, when you need today's figure; run the gate locally
when you need this checkout's.

A count in a phase-log entry (`docs/IMPLEMENTATION_PLAN.md`) is a snapshot of what that phase
measured at the time it landed, not a claim about the present, and must not be quoted as current.

Two rules hold whatever the number is. Only compare `box()=OK` counts when `FAIL: 0` — a count taken
beside a failure is not a coverage measurement. And `KRUSTY_NO_RUN=1` is for compile/emit profiling
only: it skips JVM execution, so its output must never be reported as runtime conformance.

One historical artifact is worth keeping, because the shape of the graph invites the wrong reading:
the `1842 -> 1585` cliff in `target/ir_conformance_trend.csv` was a real temporary coverage drop from
a conformance-safety cleanup, not a regression. That cleanup stopped counting unsupported shapes as
compiled support (builder-inference directives, JS-runtime-only files, advanced `Result<T>`/value-class
cases, and unsupported `UByte`/`UShort` value-class paths). Later passes recovered past both plateaus.

For corpus triage, use the survey binary through the gate profile:

```sh
./run-tests.sh --survey
./run-tests.sh --survey --parse-only --report /tmp/krusty-parse.tsv
./run-tests.sh --survey --frontend-only --report /tmp/krusty-frontend-survey.tsv
./run-tests.sh --survey --frontend-only --file coroutines/example.kt
./run-tests.sh --survey --samples "inline splice failed"
```

The parse-only TSV records exact corpus file, Kotlin block, failure stage, line, column, diagnostic,
and source line. Its summary separately reports discovered cases, Kotlin blocks, parsed cases, lex
failures, parse failures, AST failures, and panics; any failed case makes the command fail. Backend
applicability is deliberately not consulted: valid syntax must reach a complete AST even where a
later phase has no support for it, and capability diagnostics come after parsing.

The parse gate does not reach a clean sweep of the corpus, and the one case it cannot is an input
defect rather than a parser gap: `contextParameters/withExtensionReceiverInType.kt` carries extra
closing parentheses, and the pinned reference compiler reports syntax errors at exactly the same two
positions krusty rejects. The file is marked backend-inapplicable, but applicability cannot make
invalid syntax valid. Accepting it would take a corpus-path exception, silent delimiter recovery
reported as success, or a weakened invalid-syntax diagnostic, so the gate keeps the defect visible
instead. No parser change should be needed once the pinned input is corrected.

The harness builds the survey with the normal `gate` profile, applies the configurable
`KRUSTY_TEST_TIMEOUT_SECONDS` deadline, provisions the same toolchain/corpus, and reports specific
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
