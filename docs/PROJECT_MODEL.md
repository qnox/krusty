# Project model

The language server needs source roots, classpaths, module dependencies, compiler options, and a
JDK. LSP does not provide them, so `krusty-lsp` derives a `ProjectModel` from the workspace.

## Model

Each `Module` represents one compilation unit, such as a Gradle source set, Maven main/test
compilation, Android variant, Kotlin/JVM multiplatform compilation, or BSP build target. A module
contains:

- a stable build-system identifier;
- source and generated-source roots;
- resolved classpath entries;
- output, dependency, and friend paths;
- JVM target, Kotlin compiler arguments, and an optional JDK home.

Main and test compilations remain separate because their classpaths differ and test code needs the
main output as a friend path for Kotlin `internal` visibility.

The analysis worker currently consumes the union of all module classpaths. A successful model change
restarts the worker and reanalyzes open documents.

## Detection

Providers are selected in this order:

1. An explicit `-cp` argument. No build tool is run and automatic project refresh is disabled.
2. A JVM-capable `.bsp/*.json` connection.
3. Gradle markers: settings/build scripts or wrapper.
4. Maven markers: `pom.xml` or wrapper.
5. A JetBrains JPS model listed by `.idea/modules.xml`.
6. No build system. The workspace is one source root and `lib/*.jar` plus `libs/*.jar` form its
   classpath.

Gradle wins when Gradle and Maven markers occur in the same directory. Detection walks at most 16
ancestors so opening a subdirectory can still find its build root. JPS is the fallback across the
whole ancestor search; a nested `.idea` directory cannot hide a parent Gradle or Maven build.

Non-explicit providers register a common set of build-marker and local-jar watcher globs. On a
watched change the server detects the provider again, which covers adding or removing BSP, Gradle,
Maven, and local-jar markers after initialization.
JPS adds `.iml`, `misc.xml`, and project-library globs only while it is the active provider, so IDE
metadata churn cannot trigger Gradle or Maven probes.

## Gradle

The Gradle provider prefers the project wrapper and injects an init script with `--init-script`.
The script registers a `krustyModel` task and prints JSON records for:

- JVM/Java source sets and compile classpaths;
- JVM Kotlin Multiplatform compilations;
- Android variants and their platform classpaths;
- project dependencies, outputs, friend paths, JVM targets, compiler arguments, and the project
  Java toolchain.

The init script is stored in the system temporary directory. It does not edit the project.
Non-JVM multiplatform compilations are filtered by both the script and the Rust parser so native or
JavaScript roots and `.klib` files cannot enter the JVM analysis classpath.

Gradle build files are content-fingerprinted, including wrapper configuration, version catalogs,
locks, `buildSrc`, and `build-logic`. Rewriting a file with identical bytes does not run another
probe.

## Maven

The Maven provider prefers the project wrapper and runs:

```text
mvn -B --quiet dependency:build-classpath \
  -Dmdep.outputFile=.krusty-classpath.txt -Dmdep.includeScope=compile
mvn -B --quiet dependency:build-classpath \
  -Dmdep.outputFile=.krusty-classpath-test.txt -Dmdep.includeScope=test
mvn -B --quiet help:effective-pom -Doutput=<temporary-file>
```

Maven itself resolves inheritance, interpolation, settings, and active profiles in the effective
POM. Krusty does not duplicate Maven's profile activation rules. The dependency plugin provides
resolved compile and test classpaths; reactor jars are mapped to sibling output directories.
Generated source directories from prior builds are included when present. Project-model refreshes
do not run a Maven lifecycle or generate project build output.

The temporary effective POM and every `.krusty-classpath*.txt` file are removed after each probe,
including failed probes.

Maven `settings.xml` and `toolchains.xml` participate in invalidation. Maven toolchain selection is
not yet imported into `ProjectModel`; normal JDK discovery still applies.

## JPS

The JPS provider reads IntelliJ's `.idea/modules.xml`, the listed `.iml` files, project libraries,
compiler output settings, language levels, and project SDK without launching the IDE or a JVM.
Every `.iml` becomes a main module and, when test roots or test-scoped dependencies exist, a test
module. All content roots are included. Project and module libraries contribute their class roots;
module order entries become model dependencies.

IntelliJ path macros and `file:`/`jar:` URLs are resolved through the same standards-based local-file
URI parser used by other providers. Unknown macros and unavailable home-dependent macros are ignored
instead of becoming relative paths. The project SDK name is matched against JetBrains
`jdk.table.xml` files and accepted only when the resulting directory is a valid JDK home.

## BSP

The BSP provider launches the command from the selected connection file and performs:

1. `build/initialize` and `build/initialized`;
2. `workspace/buildTargets`;
3. `buildTarget/sources`;
4. `buildTarget/jvmCompileClasspath`;
5. `build/shutdown` and `build/exit`.

Only Kotlin/Java/JVM targets are requested and converted to modules. File URIs use a shared,
standards-based parser; bare paths are accepted where BSP servers emit them. Unsupported
server-to-client requests receive a JSON-RPC `Method not found` response.

The server starts a fresh BSP process for each changed fingerprint rather than keeping a long-lived
connection.

## Refresh and failure behavior

Watched-file notifications reset a 750 ms debounce window. The stdio loop uses a timed receive so
the refresh runs after writes stop even when the editor sends no further messages. The due refresh
is handled synchronously:

1. detect the provider again;
2. hash its current watched-file contents and probe version;
3. return immediately when the fingerprint matches the last successful model;
4. otherwise run the provider and restart the worker on success.

There is no background probe, cancellation, disk cache, or polling fallback.
Build-tool processes and individual BSP requests fail after two minutes.

A failed probe does not record its fingerprint and does not replace an existing model. The next
notification retries, while the last good classpath continues serving analysis. Initial failures
leave the server without a project classpath and are reported as errors.

## JDK selection

The first valid JDK wins:

1. `-jdk-home`;
2. a provider-reported project JDK;
3. `JAVA_HOME`;
4. `/usr/libexec/java_home` on macOS;
5. `java` resolved from `PATH`.

A candidate must contain `lib/modules`. If no JDK is found, the server reports one warning unless
`-no-jdk` was requested.

## Tests

Provider detection, fingerprinting, model mapping, refresh failure retention, URI handling, and
command construction are unit-tested. Gradle, Maven, JPS, and BSP parsing use recorded responses,
temporary project trees, or fake transports, so the default test suite does not require those tools
or an IntelliJ installation.

The tests do not currently execute real Gradle, Maven, Android, multiplatform, or BSP fixtures in CI.
