# krusty Maven plugin

The `verify` goal tells you whether krusty can compile your Maven build, without changing what the
build ships. kotlin-maven-plugin compiles with kotlinc as usual; the goal then compiles each of its
`compile` and `test-compile` compilations with krusty beside kotlinc's output, under
`target/krusty-verify`.

Load the plugin as a build extension, so that it can time kotlin-maven-plugin's own compile:

```xml
<build>
  <plugins>
    <plugin>
      <groupId>dev.krustythecompiler</groupId>
      <artifactId>krusty-maven-plugin</artifactId>
      <version><!-- version --></version>
      <extensions>true</extensions>
    </plugin>
  </plugins>
</build>
```

or list it in `.mvn/extensions.xml`, which leaves the POM unchanged:

```xml
<extensions>
  <extension>
    <groupId>dev.krustythecompiler</groupId>
    <artifactId>krusty-maven-plugin</artifactId>
    <version><!-- version --></version>
  </extension>
</extensions>
```

Then run the goal in the build that compiles:

```sh
mvn test-compile dev.krustythecompiler:krusty-maven-plugin:<version>:verify
```

Each release ships the plugin as `krusty-maven-plugin-<version>.zip`, a portable local Maven
repository: unpack it and add its `repository` directory as a `<pluginRepository>` (its README has
the snippet). The plugin is not on Maven Central.

Once the last project of the build is done, it writes a report to the top-level project's
`target/krusty-verify/report/`: which compilations krusty compiles, which it leaves to kotlinc and
why (an unsupported compiler plugin or kotlinc argument is reported as unsupported, not as a
failure), krusty's errors, how many classes are byte-identical to kotlinc's and how the others
differ, and krusty's and kotlinc's compile time and krusty's peak memory. kotlinc's time is that
of kotlin-maven-plugin's `compile` or `test-compile` in this build, from the start to the end of
the mojo's execution. Maven tells a goal nothing of an earlier mojo's time, so the extension times
it as it runs; without the extension, or when the compile did not run in the same build, the goal
fails and says so.

Please submit the report: its button opens a prefilled GitHub issue, or paste
`krusty-verify-issue.md`. It holds totals, difference kinds and unsupported requests with paths
masked, and no compilation, class or member name. krusty's errors are masked by rule, which cannot
promise to catch every name, so review the issue before you submit it.
`-Dkrusty.verify.shareNames=true` names compilations and keeps errors whole.

## What the goal reads

The goal reads kotlin-maven-plugin's configuration from the project model, execution settings over
plugin settings, as kotlin-maven-plugin does: `sourceDirs` (by default the compile source roots and
`src/main/kotlin` or `src/test/kotlin`), `moduleName`, `jvmTarget`, `languageVersion`,
`apiVersion`, `jdkHome`, `javaParameters`, `nowarn`, `args`, the `kotlin.compiler.*` properties,
and `compilerPlugins` with `pluginOptions`. The plugin's own dependencies, which carry its compiler
plugins (`kotlin-maven-allopen`, `kotlin-maven-noarg`, ...), are passed as `-Xplugin`. Since
`target/classes` also holds javac's classes and resources, only kotlinc's outputs (classes with
Kotlin metadata and `.kotlin_module` files) are compared. Two executions of one goal write into one
output directory, where kotlinc's output of each cannot be told apart, so each is reported
unsupported. A compilation without Kotlin sources is recorded as such, and the report holds exactly
this build's compilations. krusty refuses an output directory, record or reference that overlaps
another (also through a link) before it clears or writes anything.

## Properties

| Property | Meaning |
| --- | --- |
| `krusty.binary` | A krusty executable to run (`KRUSTY_BIN` works too). By default the goal downloads the plugin release's compiler for the host into the local repository and checks it against the release's `SHA256SUMS`. |
| `krusty.releases` | Where releases are downloaded from (a mirror of the GitHub releases). |
| `krusty.verify.shareNames` | Keep compilation names in the issue text. |

## Building

```sh
scripts/package-maven-plugin.sh <version> <output directory>   # the release zip
mvn -f tools/krusty-maven/pom.xml install -Drevision=<version>  # or into the local repository
```

The `maven_verify_goal_*` tests in `crates/krusty-build` run the goal on sample builds
(`KRUSTY_MAVEN_TEST_BIN` names the krusty to run). They use the packaged repository named by
`KRUSTY_MAVEN_PLUGIN_REPOSITORY` and `KRUSTY_MAVEN_PLUGIN_VERSION`, as CI does, or install the
plugin from the checkout.
