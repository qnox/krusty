# Top-projects conformance corpus

The `oss corpus` workflow (`.github/workflows/oss-corpus.yml`, nightly on master and on demand,
never on pull requests) builds the most-starred Kotlin projects listed in `projects.tsv`, and Now
in Android as the Android sample, each from a pinned commit with its own Gradle build:

1. **Reference:** a plain build, which compiles with kotlinc.
2. **krusty:** the same build with the krusty Gradle plugin applied by `krusty.init.gradle`. The
   plugin replaces every Kotlin/JVM compile task, including the JVM targets of multiplatform
   modules. A plugin build that rejects a multiplatform module leaves its compile tasks to
   kotlinc, and the init script reports each as a fallback. Compiler arguments are never captured
   or replayed.

`corpus.init.gradle` adds the `corpusCompile` task, which depends on every non-Android Kotlin/JVM
compile task, lists each with its output directory, and records the Gradle and compiler plugins
each project applies. After each build, `outputs.py` moves exactly the directories that build
listed out of the checkout, wherever a task writes them, after checking that each is owned by one
task inside the Gradle root; the next build therefore starts without them. `compare.py` compares
the class files of every compile task the reference build listed, byte for byte, and reports apart
a listed task kotlinc wrote no class for. A compile task the plugin leaves to kotlinc (an
unsupported compiler plugin or option) counts as a miss, with the plugin's reason; a compile krusty
fails counts as a miss with the first error it logged. Android modules take part only in a project
that names a variant in `projects.tsv`, and then only that variant's compiles.

Both builds are measured. `measure.py` runs each one as the child subreaper of everything it starts,
so the Gradle, Kotlin and krusty processes and the daemons that outlive the build are all counted: it
records wall time, CPU time, and the peak of their summed resident memory (sampled every 100 ms,
also split by kind), then stops the leftover daemons so the next build starts and pays for its own.
`corpus.init.gradle` records when each task ran, and `compare.py` adds up each compilation's compile
tasks: kotlinc's Kotlin task, and krusty's replacement, support check and Kotlin task. The two
measured builds are comparable: an unmeasured warm-up build first downloads everything the build
needs (dependencies, plugins, toolchains) and its outputs are dropped, then both measured builds run
with `--offline` from the same caches.

The krusty build is also sampled: `perf` records every process at 99 Hz while it runs (the corpus
binary keeps frame pointers), `hotspots.py` folds the krusty processes' call stacks, and
`inferno-flamegraph` draws them. `compare.py` ranks the functions krusty spends the most time in,
and the report lists the top ones across the corpus with a link to each project's flame graph.

`summarize.py` turns the per-project results into the run's report: stars, Kotlin and Java lines,
compilations built, identical classes, the reason for every miss, what both builds cost, krusty's
hotspots, and the Gradle and compiler plugins the corpus uses. The nightly run, and a manual run
with `publish` set, records the results on the `corpus-results` branch with `publish.py`:
`README.md` is the scoreboard, `projects/<name>/` holds each project's latest result (a project a
run could not measure keeps its previous one), and `history.tsv` gets one line per project per run.

Setup fails fast: a failed fetch, a checkout that is not the pinned commit, a patch that does not
apply, or a Kotlin version pin that does not match exactly one assignment (`pin.py`) stops the run
before either build. `test_compare.py`, `test_setup.py`, `test_summarize.py`, `test_publish.py`,
`test_measure.py` and `test_hotspots.py` cover the comparison, the output inventory, the pin
rewrite, the setup failures, the report, the results branch, the build measurement and the stack
folding.

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

Only the main build is measured: buildSrc and included builds are build logic, and the init scripts
leave them alone. Gradle runs without the configuration cache and isolated projects, since the init
scripts read every project of the build.

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
