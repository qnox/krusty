# Top-projects conformance corpus

The `oss corpus` workflow (`.github/workflows/oss-corpus.yml`, manual trigger only) builds the
most-starred Kotlin projects listed in `projects.tsv`, each from a pinned commit with its own
Gradle build:

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
unsupported compiler plugin or option) counts as a miss, with the plugin's reason. `summarize.py`
turns the per-project results into the run's summary table.

Setup fails fast: a failed fetch, a checkout that is not the pinned commit, a patch that does not
apply, or a Kotlin version pin that does not match exactly one assignment (`pin.py`) stops the run
before either build. `test_compare.py` and `test_setup.py` cover the comparison, the output
inventory, the pin rewrite and the setup failures.

`run.sh` rewrites each project's Kotlin version pin to the newest release in `kotlin-versions`.
Two projects carry a patch for that bump:

- `Maestro`: `JvmTarget` moved to `org.jetbrains.kotlin.gradle.dsl`, and the Android app module is
  left out of the build.
- `moshi`: kotlinc 2.4 warns that `-Xannotation-default-target=param-property` is redundant, and
  the build treats warnings as errors.

Run one project locally with:

```text
scripts/oss-corpus/run.sh <name> <work dir> <krusty binary> <plugin repository> <plugin version>
```
