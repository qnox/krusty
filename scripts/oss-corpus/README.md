# Top-projects conformance corpus

The `oss corpus` workflow (`.github/workflows/oss-corpus.yml`, manual trigger only) builds the
most-starred Kotlin projects listed in `projects.tsv`, each from a pinned commit with its own
Gradle build:

1. **Reference:** a plain build, which compiles with kotlinc.
2. **krusty:** the same build with the krusty Gradle plugin applied by `krusty.init.gradle`. The
   plugin replaces every Kotlin/JVM compile task, including the JVM targets of multiplatform
   modules. Compiler arguments are never captured or replayed.

`corpus.init.gradle` adds the `corpusCompile` task, which depends on every non-Android Kotlin/JVM
compile task, and records the Gradle and compiler plugins each project applies. `compare.py`
compares the class files of every compile task byte for byte. A compile task the plugin leaves to
kotlinc (an unsupported compiler plugin or option) counts as a miss, with the plugin's reason.
`summarize.py` turns the per-project results into the run's summary table.

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
