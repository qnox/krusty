# Top-projects conformance corpus

The `oss corpus` workflow (`.github/workflows/oss-corpus.yml`, nightly on master and on demand,
never on pull requests) builds the most-starred Kotlin projects listed in `projects.tsv`, and Now
in Android as the Android sample, each from a pinned commit with its own Gradle build. A row whose
last field is `lsp` stays in that project set for the language server (`scripts/parity-scan.py`)
and is not built here; no row is lsp-only now. `intellij-community` builds with Bazel rather than
Gradle (see "Bazel projects" below). The scan reports whether
each module opened completely, how many errors it produced, and how long the server's
go-to-definition and find-references lookups took. It does not time parsing or type information
separately from the combined analysis. Each built project runs three builds:

1. **Reference:** a plain build, which compiles with kotlinc.
2. **krusty:** the same build with the krusty Gradle plugin applied by `krusty.init.gradle`. The
   plugin replaces every Kotlin/JVM compile task, including the JVM targets of multiplatform
   modules. A plugin build that rejects a multiplatform module leaves its compile tasks to
   kotlinc, and the init script reports each as a fallback. Compiler arguments are never captured
   or replayed.
3. **Verify:** the same build in the krusty plugin's verify mode. Every compilation compiles with
   kotlinc, as in the reference build, and krusty compiles each one beside it, against kotlinc's
   classes of the compilations it depends on. A krusty failure therefore never keeps a dependent
   compilation from being compiled, and each compilation's record holds every error krusty
   reported. The verify build exists for those errors; its classes and cost are not compared.

`corpus.init.gradle` adds the `corpusCompile` task, which depends on every non-Android Kotlin/JVM
compile task (or, for a project with a target set, the ones the set needs: see below), lists each with its output directory, and records the Gradle and compiler plugins
each project applies. After each build, `outputs.py` moves exactly the directories that build
listed out of the checkout, wherever a task writes them, after checking that each is owned by one
task inside the Gradle root; the next build therefore starts without them. `compare.py` compares
the class files of every compile task the reference build listed, byte for byte, and reports apart
a listed task kotlinc wrote no class for. A compile task the plugin leaves to kotlinc (an
unsupported compiler plugin or option) counts as a miss, with the plugin's reason; a compile krusty
fails counts as a miss with the first error it logged. A compile that did not run because a task it
depends on failed names the failed tasks that stopped it (`blocked_by`), traced through the task
graph the init script writes to `$CORPUS_TASK_DEPS`. With `$CORPUS_VERIFY_RECORDS` set,
`corpusCompile` also runs the plugin's verify compile (`<task>KrustyVerify`) of each listed task
and lists the record each writes; `run.sh` names `krustyVerify`, which turns verify mode on, and
excludes it, so only the listed compilations are verified rather than every module and variant.
`compare.py` adds each compilation's verify outcome and all of its errors. Android modules take part only in a project
that names a variant in `projects.tsv`, and then only that variant's compiles. A comma-separated
list names, in order, the variant each Android module contributes: the first one it has, so an
app's flavored variant and the build type of the library modules it consumes are compared together.

After each measured build, an unmeasured test build runs the project's own JVM tests on the classes
that build compiled. `outputs.py --keep` first snapshots the measured outputs and leaves them in
place for it; `outputs.py --clear` then removes them and lists every file under them the test build
wrote, rewrote or deleted (a compile task that ran again, say). The snapshot, and so the byte
comparison, is always the measured build's; when a test build changed it, its tests did not run on
the measured classes, and the project's test comparison is reported unavailable instead. `corpus.init.gradle` adds the
`corpusTest` task, which depends on every JVM `Test` task of the projects whose compiles are
compared (an Android project's unit tests of its variant only). In a test build no test task is up
to date or taken from the build cache, a failing test does not fail its task, and the init script
lists each test task with its JUnit XML report directory and the compile tasks it depends on,
directly or through an upstream module's jar. Test builds run online, since the warm-up downloads
only what compiling needs, and stop after `$CORPUS_TEST_TIMEOUT` seconds (3600 by default).
`testruns.py` compares only the tests that passed on kotlinc's classes, and counts how many of them
pass, fail, are skipped or do not run on krusty's; a test task whose compilations krusty built
every one of runs krusty's classes only, and those tasks are totalled apart.

Both builds are measured. `measure.py` runs each one as the child subreaper of everything it starts,
so the Gradle, Kotlin and krusty processes and the daemons that outlive the build are all counted: it
records wall time, CPU time, and the peak of their summed resident memory (sampled every 100 ms,
also split by kind), then stops the leftover daemons so the next build starts and pays for its own.
`corpus.init.gradle` records when each task ran, and `compare.py` adds up each compilation's compile
tasks: kotlinc's Kotlin task, and krusty's replacement, support check and Kotlin task. The two
measured builds are comparable: an unmeasured warm-up build first downloads everything the build
needs (dependencies, plugins, toolchains) and its outputs are dropped, then both measured builds run
with `--offline` from the same caches. A warm-up that fails to download something is not retried (a repository extends a block for every
request made during it); it stops the run with a pointer to its log. Before it stops, the repositories
the log names, and the default ones, are probed: `<name>.warmup.http.tsv` and stderr list their HTTP
status (`000` for a host that does not answer), because Gradle reports any refused download as "not
found".

A project's job fails when the reference kotlinc build or the krusty build fails, after the
comparison has recorded how far each got, so the project still appears in the report; the `summary`
job runs either way.

The krusty build is also sampled: `perf` records every process at 99 Hz while it runs (the corpus
binary keeps frame pointers), `hotspots.py` folds the krusty processes' call stacks, and
`inferno-flamegraph` draws them. `compare.py` ranks the functions krusty spends the most time in,
and the report lists the top ones across the corpus with a link to each project's flame graph.

`summarize.py` turns the per-project results into the run's report: stars, Kotlin and Java lines,
compilations built, identical classes, the failed tasks ranked by how many compilations each kept
from running, the reason for every miss, every error the verify build
reported (grouped by cause: the message without its location, with quoted text and numbers masked,
counted across the corpus and listed in full per project; a project whose verify build recorded
nothing is reported as unverified, not left out), the projects' tests that pass on krusty's classes,
what both builds cost, krusty's hotspots, and the Gradle and compiler plugins the corpus uses.
Each `corpus-<project>` workflow artifact holds that project's comparison and logs. The `summary`
artifact holds `summary.md` and `summary.json` for the results produced by this run; the job summary
displays the same Markdown report, except that the page refuses more than 1 MiB: the per-project error
listings, largest first, are then reduced to one line that points at `summary.md`. No table cell is empty: a project whose job left no comparison has no
row and is listed under "Not measured" with why (the first failed build, Gradle's cause line, and the HTTP
status of a failed download, read from the logs its job uploaded), a test run that cannot be compared is
listed under its table, and totals are sentences below the tables. The run itself fails while any project
is not measured, after the summary is published.

To work on one failure, `repro.sh <project> <compile task path>` (for example
`repro.sh ktor :ktor-io:compileKotlinJvm`) checks the project out the way the nightly does and
compiles that one compilation the way the verify build does: kotlinc compiles it and everything it
depends on, and krusty compiles it beside kotlinc against kotlinc's classes. It then prints every
error krusty reported. It builds krusty and the plugin from the repository unless
`$KRUSTY_BINARY` and `$KRUSTY_PLUGIN_REPOSITORY`/`$KRUSTY_PLUGIN_VERSION` name prebuilt ones, and it
reuses its checkout, so a rerun after a krusty change costs one krusty build and one compilation.

Setup fails fast: a failed fetch, a checkout that is not the pinned commit, a patch that does not
apply, or a Kotlin version pin that does not match exactly one assignment (`pin.py`) stops the run
before either build. A reference build that fails is still compared as far as it got, and then
fails the project's job: every listed project must build with kotlinc, so a failure there is fixed
with a patch or a pin, never left to shrink what the corpus measures. `test_compare.py`, `test_setup.py`, `test_summarize.py`, `test_workflow.py`,
`test_measure.py`, `test_hotspots.py` and `test_testruns.py` cover the comparison, the test runs,
the output inventory, the pin rewrite, the setup failures, the report artifacts, the build
measurement and the stack folding.

`run.sh` rewrites each project's Kotlin version pin to the newest release in `kotlin-versions`.
These projects carry a patch for that bump:

- `Maestro`: `JvmTarget` moved to `org.jetbrains.kotlin.gradle.dsl`, and the Android app module is
  left out of the build.
- `mirai`: its build was written for Kotlin 1.8. The patch moves it to Gradle 8.2.1 and atomicfu
  0.33 (whose Gradle plugin applies on Kotlin 2.4), ports its build logic and its console Gradle
  plugin to the `compilerOptions` DSL, and fixes the sources kotlinc 2.4 rejects (variance in an
  inner class, expect/actual pairs, data-class `copy` visibility, `INVISIBLE_REFERENCE`
  suppressions, a hand-written `$serializer` that crashes the serialization plugin). Tests
  compile at the default language version and without `ContextReceivers` (no test uses them; the
  build enabled them through `enableLanguageFeature`, an error in Kotlin 2.4's Gradle DSL), and
  `mirai-core-utils` skips atomicfu's bytecode transform as its siblings already do. The
  blocking-bridge and dynamic-delegation compiler plugins have no Kotlin 2 release and are left
  out, as are the Android target and the IntelliJ plugin module (`local.properties`). The version
  pin is `kotlinCompiler` in `buildSrc/src/main/kotlin/Versions.kt`.
- `moshi`: kotlinc 2.4 warns that `-Xannotation-default-target=param-property` is redundant, and
  the build treats warnings as errors.
- `kotlinx.coroutines` and `ktor`: Kotlin 2.4 removed the ABI validation DSL their build logic
  uses, so ABI validation is left out. kotlinx.coroutines also stops treating Kotlin 2.4
  deprecation warnings as errors, and ktor drops an override of the removed `withWasm()`.
- `okhttp`: Kotlin 2.4 infers `kotlin-test-junit5` beside the declared `kotlin-test-junit`; the
  inference is turned off.
- `okhttp` and `okio`: the Burst compiler plugin moves to a release built for Kotlin 2.4.
- `kotlin`: the build compiles itself with a bootstrap compiler, by default a development build
  from JetBrains' bootstrap repository. The patch takes the bootstrap from Maven Central instead, at
  the version the pin (`bootstrap.kotlin.version` in `gradle.properties`) names, and turns Gradle's
  dependency verification off, since its metadata lists only the default bootstrap's artifacts. The
  pinned commit is the v2.4.21 tag, whose sources are Kotlin 2.4.

A project too large to build whole on one runner names a target set in `gradle/<name>.targets`: Gradle
task paths, one per line. `corpusCompile` then builds only the listed tasks that are not tests, the
compiles picked are those the build runs for them (the targets and every compile they depend on),
and `corpusTest` runs only the listed test tasks. `kotlin` lists its JVM standard library and the Compose compiler
plugin (`compiler-hosted`, a kotlinc plugin built on the compiler's FIR and IR APIs), each with its
tests' compile, and its whole JVM compiler (`:compiler:cli-jvm`), and runs the standard library's
JVM tests.

Every Gradle build is offered each JDK the runner installed (`JAVA_HOME_<version>_X64`) as a Java
toolchain (`toolchains.sh`), since Gradle does not look there by itself. `kotlin` compiles its modules
with JDK 8, 11 and 17 toolchains, and its build logic cannot download one.

Only the main build is measured: buildSrc and included builds are build logic, and the init scripts
leave them alone. Gradle runs without the configuration cache and isolated projects, since the init
scripts read every project of the build.

## Bazel projects

A project with a `bazel/<name>/` directory builds with Bazel: `run.sh` hands it to `bazel.sh`.
`bazel/<name>/targets` lists the targets built, and `bazel/<name>/integration` names the directory of
this repository whose patches route the project's Kotlin compile actions to krusty
(`tools/krusty-bazel/intellij` for intellij-community, see `tools/krusty-bazel/intellij.md`). The
patches are applied after checkout and change nothing until a build passes the krusty binary as
`--repo_env=KRUSTY_BINARY=…`. An optional `bazel/<name>/checkouts` pins further repositories the build
reads, cloned into the checkout first (intellij-community's project model reads the Android plugin,
`android/`). The reference build is the project's own (`JvmCompile`, its builder
with the embedded kotlinc), and in the krusty build each Kotlin-only action without source jars or
compiler plugins is a `KrustyCompile` while the rest stay on the project's builder and count as
misses with that reason. intellij-community is pinned at the last revision built with Kotlin 2.4.20
exactly, and its target set is the Kotlin-only targets whose closures are smallest, up to about
10,000 source files in all, so both builds fit one runner job.

A warm-up (`build --nobuild`) fetches every repository; each measured build starts from `bazel clean`
and a stopped server, without a disk cache and with downloads disabled, under `measure.py`. Its
execution log lists every compile action, from which `bazel_compare.py` copies the jars out of the
execution root and writes `compare.json` in the Gradle shape: one compilation per compile action,
named by its target's label (with ` [for tool]` for a target also built for the execution platform,
as Bazel's progress messages say), its class entries compared byte for byte. As for a Gradle
project, a failed reference build is compared as far as it got and then fails the run. There is no verify build, test build or sampled profile.

Run one project locally with:

```text
scripts/oss-corpus/run.sh <name> <work dir> <krusty binary> <plugin repository> <plugin version>
```

## Warm benchmark

The corpus builds above are cold: each starts without a Gradle or Kotlin daemon and pays for its
own JVM warm-up. A developer's build does not, so `bench.sh` measures what an edit loop costs with
a warm Gradle daemon, with kotlinc running in the Kotlin daemon with incremental compilation, the
way the Kotlin Gradle plugin runs it by default:

```text
scripts/oss-corpus/bench.sh <name> <work dir> <krusty binary> <plugin repository> <plugin version>
```

A krusty probe build first finds the compilations krusty builds today (`bench.py plan`): only those
are benchmarked, both compilers build exactly that task set, and both builds must succeed. Each
compiler then gets its own session against its own Gradle and Kotlin daemons (`bench.py session`),
which it starts under a Gradle user home and Kotlin daemon directory of its own, checks that every
build ran in, and stops, leaving any other daemon on the machine alone: unrecorded
warm-up builds, clean builds (`--rerun-tasks`), and an edit loop that changes one Kotlin file
between builds, first without changing its ABI (the body of a private function) and then with an
ABI change (a public function renamed). The file is the median-sized source of the benchmarked
main compilation with the most sources, or `$BENCH_EDIT`. Recorded builds run offline and without
the build cache. `$BENCH_WARMUPS`, `$BENCH_CLEAN_RUNS` and `$BENCH_EDIT_RUNS` set the run counts.

Each build records its wall time, the time of the benchmarked compile tasks, the peak of the summed
resident memory of every process the session started (the warm daemons included, sampled every
100 ms, by kind) and what stays resident afterwards. `bench.py report` writes
`<work dir>/bench/<name>.json` with the median times and highest peak memory of each scenario and
prints a Markdown table. krusty compiles a whole compilation each time its task is out of date,
while kotlinc recompiles only what changed: the edit loop measures that difference as it is.
