# krusty Maven plugin

The `verify` goal tells you whether krusty can compile your Maven build, without changing what the
build ships. kotlin-maven-plugin compiles with kotlinc as usual; the goal then compiles each of its
`compile` and `test-compile` compilations with krusty beside kotlinc's output, under
`target/krusty-verify`:

```sh
mvn test-compile dev.krustythecompiler:krusty-maven-plugin:<version>:verify
```

Once the last project of the build is done, it writes a report to the top-level project's
`target/krusty-verify/report/`: which compilations krusty compiles, which it leaves to kotlinc and
why (an unsupported compiler plugin or kotlinc argument is reported as unsupported, not as a
failure), krusty's errors, how many classes are byte-identical to kotlinc's and how the others
differ, and krusty's compile time and peak memory. Maven does not time kotlin-maven-plugin's
goals, so the report has no kotlinc time.

Please submit the report: its button opens a prefilled GitHub issue, or paste
`krusty-verify-issue.md`. It is anonymized: totals, difference kinds, unsupported requests with
paths masked, and the shape of krusty's errors without their locations or the identifiers they
quote. `-Dkrusty.verify.shareNames=true` keeps compilation names in it.

## What the goal reads

The goal reads kotlin-maven-plugin's configuration from the project model, execution settings over
plugin settings, as kotlin-maven-plugin does: `sourceDirs` (by default the compile source roots and
`src/main/kotlin` or `src/test/kotlin`), `moduleName`, `jvmTarget`, `languageVersion`,
`apiVersion`, `jdkHome`, `javaParameters`, `nowarn`, `args`, the `kotlin.compiler.*` properties,
and `compilerPlugins` with `pluginOptions`. The plugin's own dependencies, which carry its compiler
plugins (`kotlin-maven-allopen`, `kotlin-maven-noarg`, ...), are passed as `-Xplugin`. Since
`target/classes` also holds javac's classes and resources, only kotlinc's outputs (classes with
Kotlin metadata and `.kotlin_module` files) are compared.

## Properties

| Property | Meaning |
| --- | --- |
| `krusty.binary` | A krusty executable to run (`KRUSTY_BIN` works too). By default the goal downloads the plugin release's compiler for the host into the local repository and checks it against the release's `SHA256SUMS`. |
| `krusty.releases` | Where releases are downloaded from (a mirror of the GitHub releases). |
| `krusty.verify.shareNames` | Keep compilation names in the issue text. |

## Building

```sh
mvn -f tools/krusty-maven/pom.xml install -Drevision=<version>
```

`maven_verify_goal_reports_krusty_against_kotlinc` in `crates/krusty-build` installs the plugin and
runs the goal on a sample build (`KRUSTY_MAVEN_TEST_BIN` names the krusty to run).
