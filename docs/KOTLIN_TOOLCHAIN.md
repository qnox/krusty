# Kotlin Toolchain projects

`crates/krusty-toolchain` reads projects in the Kotlin Toolchain format (`project.yaml`,
`module.yaml`), the format JetBrains' `kotlin` command (formerly Amper) builds. The goal is a
drop-in replacement for that command that compiles with krusty, the way krusty replaces `kotlinc`.
The reference is Kotlin Toolchain 0.13.0.

## What is implemented

- `krusty-toolchain [--project-dir=<path>] show modules [--format=plain|table]` prints exactly what
  `kotlin show modules` prints.
- `krusty-toolchain [--project-dir=<path>] show settings [-m <module>]... [--all-modules]` prints
  exactly what `kotlin show settings` prints for a JVM module's main fragment: every setting with
  where its value comes from (a file, the default, or a default derived from another setting).
- `krusty-toolchain [--project-dir=<path>] show dependencies [-m <module>]... [--all-modules]
  [--include-tests]` prints exactly what `kotlin show dependencies` prints: each module's resolved
  dependency graph for the compile and runtime classpaths of its main (and test) fragment. See
  [Dependency resolution](#dependency-resolution).
- `krusty-toolchain [--project-dir=<path>] task :<module>:compileJvm...` compiles the modules' main
  fragments and those of the modules they compile against, each once, with the krusty binary
  `KRUSTY_BIN` names, passing it the compiler arguments the toolchain's compilation passes. See
  [Compiling](#compiling).
- Project discovery: the nearest directory at or above the current one with a `project.yaml`. A
  module file passed on the way that the project does not list makes its directory a single-module
  project.
- `project.yaml`: `modules` (plain paths and globs) and `plugins`.
- `module.yaml` and the templates it applies (`apply`, transitively): `product`, `description`,
  `dependencies` and `settings`, with `test-` and `@jvm` variants, read against the toolchain's
  schema (`src/schema`). Of `repositories`, only `mavenLocal` is used; `layout` chooses where a
  compiled module's sources and resources are; `tasks` is read but not yet used; `plugins` is read
  when a module is compiled (see [Toolchain plugins](#toolchain-plugins)); `mavenPlugins`,
  `aliases` and `pluginInfo` are accepted without being read.

## Settings

A module's files are read into value trees (`src/tree`): each value knows the file it is written
in, whether it is a test value, and whether it is qualified `@jvm`. Refinement merges the trees as
one fragment sees them, as the toolchain's `TreeRefiner` does: the most specific value of a
property wins (the module beats its templates, a template beats the templates it applies, `@jvm`
beats unqualified, a test value beats a main one in the test fragment); lists concatenate and
objects merge, least specific first; equally specific different values are a conflict. Defaults
fill what nothing set, defaults that name another setting are resolved after refinement, and an
object missing a required property is reported and dropped.

The module is refined as a whole and then as its main and test fragments, as the toolchain does,
and each conflict is reported once. Settings written where they have no effect are reported as the
toolchain reports them: a platform-agnostic setting under `@jvm`, a setting for another platform,
and an application setting in a library. The `android` section is accepted without being read,
since krusty-toolchain builds no Android modules and the toolchain only warns about it on a JVM
module.

Versions compare as Maven compares them (`maven_version.rs`), as the toolchain does for
`compileIncrementally`.

## Reading rules

- YAML is parsed by `saphyr-parser` (YAML 1.2). Anchors, aliases, `!!` tags, custom `!` tags
  (except on a file's top mapping) and a second document are reported with the toolchain's
  messages. A YAML syntax error is reported with the parser's
  message where the toolchain's PSI parser would recover.
- Module globs follow `java.nio` `glob:` semantics (`sun.nio.fs.Globs`) after the toolchain's
  normalisation; `glob.rs` is checked against the JDK matcher on every case in
  `tests/cases/globs.tsv` (see "Differential tests"). Matches are sorted.
- Every file-system look and read goes through `inventory.rs`: bounded, sorted, and never through a
  symbolic link. A link met on the way to a module, inside a walked region, or as a build file
  (`project.yaml`, `module.yaml`, `project.amper`, `module.amper`, `plugin.yaml`) is an error naming
  it. A build file is opened without following a link (`O_NOFOLLOW`) and read from that handle, so
  it cannot become a link between being found and being read. Each directory's entries are sorted
  before limits apply, so the error a walk reports does not depend on the file system's order.
- A `modules` path is walked component by component as the file system walks it, so `a/x/../b` is
  unresolved when `a/x` does not exist, as it is for the toolchain.
- Diagnostics are `(severity, message, file, line, column)` in the toolchain's order and words:
  within a file, a value's problems where the value is met and unknown properties last; a module
  file without `product` reports only that.
  Project-file errors stop before module files are read; module-file errors stop before module names
  are compared.

## Dependency resolution

`src/maven` reads artifacts and `src/resolution` resolves graphs, ported from the toolchain's
`dependency-resolution` and `frontend/dr`:

- Artifacts are read from the toolchain's shared cache, `<cache root>/.m2.cache` (the cache root is
  `KOTLIN_SHARED_CACHE_DIR`, else the platform's user cache directory followed by
  `JetBrains/Kotlin`), and, when a module lists `mavenLocal` in its `repositories`, first from the
  local Maven repository (`<localRepository>` of `~/.m2/settings.xml` or
  `$M2_HOME/conf/settings.xml`, else `~/.m2/repository`). Only an absent settings file, or one
  without `<localRepository>`, falls through to the next; one that cannot be read, is not
  well-formed, or sets an empty location or one through a property is an error, where the
  toolchain silently reads another repository. `mavenLocal` applies to the whole project. A file
  is read from the first repository that has it; one that is there but cannot be read is a
  problem, not a reason to read the next. Coordinates name a file only when each part
  (every group segment, the artifact, version and classifier) is one file name: not empty, `.` or
  `..`, and without `/`, `\`, `:` or NUL.
- Every artifact in a printed graph must be read completely. An artifact in no repository
  (krusty-toolchain does not download yet), metadata that does not parse, a classpath no variant
  or more than one variant matches, and coordinates that name no file are errors, reported with
  the metadata file and, when its parser says, the line and column, module by module and in the
  order the graphs meet them, and `show dependencies` then prints no graph and fails. The
  toolchain downloads what is missing and, for the rest, logs a warning or nothing and prints the
  graph; these are deliberate differences.
- An artifact's dependencies come from its Gradle module metadata when its POM carries the
  `published-with-gradle-metadata` marker or it has no POM (the JVM variant for the classpath,
  with platform dependencies, `available-at` and constraints), else from its effective POM
  (parents, profiles for the module's JDK version, properties, imported BOMs, dependency
  management, and the POM scopes of each classpath).
- A graph is resolved in waves: within a wave, nodes requesting different versions of one
  `group:module` are a conflict and are not expanded; between waves, every such node aligns on the
  highest version (Maven's `ComparableVersion`), dependencies declared without a version take the
  version their module's BOMs manage, and the nodes that changed are resolved again.
- Each module is resolved on its own: its main and test fragments, each as one graph holding the
  compile and the runtime classpath, with the modules it depends on, what they export to its
  compile classpath and all they need at runtime, and the implicit dependencies the toolchain adds
  (the standard library, the test framework, and the runtime libraries of enabled features).
- What an artifact declares is read once per run for every module that reads it alike (the same
  JDK version and `excludeDependencies`), and parsed POMs, effective POMs and module metadata
  once per run.

## Compiling

`task :<module>:compileJvm...` builds its whole plan before compiling anything
(`src/compile/plan.rs`): the modules named, as `kotlin task` takes several task names, and the
modules they depend on for compilation, transitively, each once, in dependency order with the
project's module order breaking ties, each with its krusty arguments in the order the
toolchain's compilation passes them:

```text
-Xjdk-release=<settings.jvm.release> -jdk-home=<JDK> -classpath=<classpath> -no-stdlib
-module-name=<module> [-java-parameters]
[-Xmulti-platform -Xfragments=main -Xfragment-sources=main:<source>...]
[-Xplugin=<kotlin-serialization-compiler-plugin jar>]
-Xkotlin-reference-version=<settings.kotlin.version>
-d <root>/build/artifacts/CompiledJvmArtifact/<module>jvm/kotlin-output <sources, sorted>
```

The main fragment's source directories are the layout's (`src`, or `src/main/java` then
`src/main/kotlin` in the Maven-like layout) followed by those its toolchain plugins add. The
toolchain counts the ones that are directories holding anything at all (a file of any kind); when
more than one does, it describes the fragment to the compiler: `-Xmulti-platform`,
`-Xfragments=main`, and a `-Xfragment-sources=main:<file>` for each `.kt` and `.java` file in the
order it walks the directories (each in turn, depth first, entries in the order the file system
lists them), which krusty-toolchain walks the same way. A source directory that exists without
being a directory is refused with the toolchain's message.

The classpath is the module's resolved compile graph, walked breadth first
(`src/compile/classpath.rs`): first each module dependency's compiled output (when it has Kotlin
sources) followed by its resources output (when it has resources), then each artifact's files,
each once. The JDK is the one `JAVA_HOME` names; it must be a JDK (it has `bin/javac`) of the major
version `settings.jvm.jdk.version` asks for. Artifact files and the serialization plugin come from
the local repositories only. A module without Kotlin sources is not compiled. For each module the
task clears its output directory, runs `KRUSTY_BIN` with the arguments, and copies the resources
directory (`src/main/resources` in the Maven-like layout, else `resources`), then each resource
directory its toolchain plugins add, to `resources-output`; a file of a later directory replaces
the same file of an earlier one, as the toolchain's does. A module whose only resources a plugin
adds still puts `resources-output` on its dependents' classpath. A resource directory that holds
the module's `resources-output` (a plugin directory at the root of a root module, say) is refused,
as copying it would copy the output into itself.
A module is not compiled again when its inputs and its output are as its last successful
compilation left them (`src/compile/avoidance.rs`), the check krusty's Build Tools API
implementation makes for the toolchain: krusty fingerprints the inputs
(`krusty compilation-fingerprint` before the arguments), and the output directory is compared by its
files' paths and contents with the state recorded in
`<root>/build/krusty-toolchain/<module>jvm/compilation.state`. A body-only edit of a module
therefore recompiles only that module. When krusty takes no fingerprint, the module compiles.
Modules run as the toolchain runs its task graph (`src/compile/schedule.rs`): a module starts as
soon as the modules it compiles against have finished, at most as many at once as the host has
available processors (the toolchain bounds its task executor the same way and has no option for
it, so neither does krusty-toolchain). krusty's output is captured per module and printed whole in plan
order, as soon as the module and every module planned before it that ran have finished, so each
module's output stays together and nothing printed depends on which compilation finishes first.
Once a module fails no module begins: a module begins, before it touches its outputs, only under
the same lock a failure is recorded under, so it either began before the failure or does nothing.
The running ones are waited for, each failure is reported in plan order, and the task exits with the status of
the first failing krusty in plan order. When several tasks name modules that do not exist, the last is reported, as the
toolchain reports it.

Before compiling anything, krusty-toolchain refuses, at the setting and naming the module, every
module of the plan that:

- enables a toolchain plugin it does not implement (see [Toolchain plugins](#toolchain-plugins)) or
  runs a KSP processor that is not a Maven artifact (see [KSP](#ksp));
- sets a compiler setting it does not pass yet (`src/compile/refusals.rs`: the language and API
  versions, free compiler arguments, opt-ins, language features, warning options, explicit API
  mode, `debug`, compiler plugins and the settings the toolchain turns into compiler plugins);
- has Java sources;
- needs a JDK provisioned (`selectionMode: alwaysProvision`, `distributions`, no `JAVA_HOME`, or a
  `JAVA_HOME` that is not a JDK of the version asked for);
- needs an artifact downloaded;

and refuses compile dependencies that form a cycle. It refuses an unset `KRUSTY_BIN` rather than
searching `PATH`, and every task other than `compileJvm`.

### KSP

A module whose `settings.kotlin.ksp.processors` names Maven artifacts runs KSP before it compiles,
as the toolchain's `kspJvm` task does (`src/compile/ksp.rs`): `<JDK>/bin/java @<argument file>`
runs `com.google.devtools.ksp.cmdline.KSPJvmMain` with KSP's artifacts (`symbol-processing-api`,
`symbol-processing-aa-embeddable` and `symbol-processing-common-deps` of `ksp.version`, resolved
with their dependencies) on the class path, the module's existing source directories, its compile
classpath as `-libraries`, `ksp.processorOptions` in the order written, and the processors'
runtime classpath as the last argument, every argument as the toolchain passes it. KSP writes below
`build/generated/<module>/main` and keeps its caches and temporary files below
`build/tasks/_<module>_kspJvm`. The module then compiles with KSP's generated classes as the last
classpath entry, and its resources output takes KSP's generated resources after its other resource
directories, so a module running KSP always puts a resources output on its dependents' classpath.
KSP runs on every task run. A run that generates Kotlin or Java sources fails the module, since
krusty-toolchain does not compile generated sources yet.

### Toolchain plugins

A module enables a plugin by name in `plugins:` (`<name>: enabled` or `<name>: {enabled: true}`);
the name is that of a local plugin module the project lists in its own `plugins:`
(`src/compile/plugins.rs`). krusty-toolchain implements the plugins whose `plugin.yaml` only adds
Kotlin source directories and resource directories of the module to its main fragment:

```yaml
tasks:
  declare:
    action: !org.example.declareSources
      sourceDir: ${module.rootDir}/generated-src/kotlin
      resourceDir: ${module.rootDir}/generated-resources
generated:
  sources:
    - language: kotlin
      directory: ${tasks.declare.action.sourceDir}
  resources:
    - directory: ${tasks.declare.action.resourceDir}
```

A directory is the argument of a task's action that is `${module.rootDir}` or a plain path below
it, or that directory written out, which the toolchain takes as that task's output. A directory
written out that no task's action argument is the toolchain rejects, for each module that enables
the plugin, and krusty-toolchain reports the same problem. krusty-toolchain runs no plugin task: the action is the plugin's own code, so a directory a
task provides is compiled as it is on disk, and refused when it is not a directory there. It does
not compile the plugin module before the modules that enable it, as the toolchain does to run the
task. It refuses, where each is written: a plugin the project does not list, plugin settings
beyond `enabled`, `plugins` written in more than one of a module's files, `pluginInfo` in the
plugin's module file, and everything else a `plugin.yaml` may declare (`checks`, `commands`,
`module`, `project`, `pluginSettings`, a task's `markOutputsAs` or `taskOutputDir`, cinterop
definitions, generated sources in another language or for another fragment, anything but a
`directory` on generated resources, and directories written any other way).

The library's public surface is the command boundary: `model` (read a project), `configuration`,
`compile`, `dependencies`, `diagnostic` and `show`. Discovery, the file-system inventory, YAML,
the build-file readers, Maven metadata and resolution are private.

## Deliberate refusals

krusty-toolchain refuses, with an error naming itself:

- `project.amper` and `module.amper` files;
- a `modules` entry with a leading `/` or `//` (the toolchain accepts `//path`);
- `mavenPlugins` in `project.yaml`;
- product types other than `jvm/app`, `jvm/lib` and `jvm/amper-plugin`, and a JVM product whose
  `platforms` is anything but `[jvm]`;
- a qualifier other than `@jvm` on `dependencies`/`settings`, and any qualifier on another property.

## Differential tests

JetBrains' `kotlin` is the oracle, used the way krusty's tests use `kotlinc`: the repository holds
only the inputs, and the reference's output is cached. `tests/cases/projects/*.case` hold a
project's files (and, in a `--- krusty` section, what krusty-toolchain reports where it
deliberately differs; that must be a refusal in krusty-toolchain's name, or the toolchain must
reject the case too). `tests/project_cases.rs` runs `kotlin show modules` on each case through the
toolchain's own wrapper, `scripts/kotlin-toolchain/kotlin`, which pins the exact distribution, and
requires krusty-toolchain to report the same problems (file, line, column, severity, message) in
the same order: the errors the toolchain writes to stderr, which must hold nothing else, and the
warnings it writes to stdout. What stdout holds after the warnings is the command's result, and
must equal krusty-toolchain's module table byte for byte (nothing, when the command fails).
Module globs are checked the same way against the JDK matcher: `tests/cases/globs.tsv` lists
patterns and paths, and `scripts/kotlin-toolchain/GlobOracle.java` judges them on the JDK
`JAVA_HOME` names.

`tests/cases/settings/*.case` are checked the same way with `kotlin show settings --all-modules`:
`tests/settings_cases.rs` requires the same problems and requires what stdout holds after the
warnings to equal krusty-toolchain's settings byte for byte, including the line naming each module,
which the toolchain pads to 1,500 columns. Strings are printed as the toolchain's `YamlSerializer`
prints them: a value as it is, and a free-form map's key as written in the file, inside double
quotes, neither escaped (`tests/cases/settings/hostile-strings.case`). Keys are matched as written
too: a quoted key keeps its escapes.

Whole commands are compared too: `tests/support/command.rs` runs `krusty-toolchain` and `kotlin`
with the same arguments in the same case and requires the same exit status, stdout and stderr,
byte for byte, once each run's project directory is replaced by one placeholder. `show modules`
(`tests/project_cases.rs`) is compared on a clean project in each format, a warning, an error in the
project file, an error in a module file and two modules with one name; `show settings`
(`tests/settings_cases.rs`) on a clean project with and without `-m`, warnings, errors, and
`--all-modules`, `-m`, no selection and an unknown module among several. This holds krusty-toolchain
to the toolchain's rendering (`src/report.rs`, after its `RichTerminalProblemReporter`): a problem at
a place is a box quoting the source with the place marked, files are named as they are while the
project file is read and relative to the project root after, and a command that stops says why on
a last `ERROR:` line set apart by a blank line.

`tests/cases/dependencies/*.case` are checked with `kotlin show dependencies --all-modules
--include-tests` on a project that resolves from a local Maven repository (`mavenLocal`):
`tests/cases/dependencies/repository` (the standard library and the test framework) with the
case's `m2/<path>` sections added, in the home directory both are given (`HOME` for
krusty-toolchain, `-Duser.home` for the toolchain's JVM), with a fresh `KOTLIN_SHARED_CACHE_DIR`
and an unreachable proxy, so every artifact a case resolves is in that repository.
`tests/dependency_cases.rs` runs `krusty-toolchain` in the same layout and requires the same exit
status, stdout and stderr, byte for byte, once each run's own directory is replaced by the same
placeholder. Where krusty-toolchain refuses an artifact it cannot read completely, the case's
`--- krusty-refusal` section holds its errors, `error` and a tab before each one in the form the
toolchain's problems are read back in (`tests/support/rendering.rs`): read back the same way, they
must be its complete stderr, with exit status 1 and nothing on stdout.

`tests/cases/compile/*.case` run a `--- task` in the same layout, with a JDK beside the project and
a `KRUSTY_BIN` that only records its arguments (`tests/support/compiling.rs`).
`tests/compile_cases.rs` requires krusty-toolchain to record exactly the case's `--- krusty-argv`
section (each invocation's arguments, then `----`, each run's directory replaced by a placeholder),
with exit status 0 and no output, or to report exactly its `--- krusty-refusal` section and record
nothing. The toolchain records the same arguments through krusty's Build Tools API implementation
(`tools/krusty-build-tools`), built from its sources (by the JDK `KRUSTY_BUILD_TOOLS_JAVA_HOME`
names, else `JAVA_HOME`) and installed into its shared cache, and must record each argv section
(compared module by module, sources as a sorted list). The toolchain compiles a toolchain plugin's
module and runs its tasks before the modules that enable it, so a case with a plugin names the
plugin's module in a `--- toolchain-plugin-module` section: the recorder then also compiles that
module, with the reference Kotlin compiler (the case's local repository lists the plugin API, whose
stand-in jar is replaced by the one the distribution ships, and the standard library's by the
reference compiler's), and the toolchain's invocation for it, which krusty-toolchain does not make,
must be recorded once and is left out of the comparison. A case with neither section is a project
both reject: krusty-toolchain must refuse it, and where the oracle runs, with exactly the problems
the toolchain reports.

`tests/support/oracle.rs` caches each reference's exit code and raw stdout and stderr (only a JVM's
`Picked up JAVA_TOOL_OPTIONS` line is dropped from stderr) under
`target/cache/kotlin-toolchain-oracle` (or `KRUSTY_TOOLCHAIN_ORACLE_DIR`), keyed by the reference's
identity (the wrapper's bytes, or the JDK's `release` record and `GlobOracle.java`) and every
input. A cached entry is replayed. A missing one fails locally; record it from the reference with

```text
KRUSTY_RECORD=1 cargo test -p krusty-toolchain
```

(`KRUSTY_RECORD=1` re-runs every entry; `KRUSTY_TOOLCHAIN_RUN_MISSING=1` runs only missing ones).
The `ci` job restores the cache master saved, runs the references for whatever is missing, and
master saves the result, so a new case or a new toolchain version is checked against the live
toolchain without running it for every case on every pull request.
