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
- Project discovery: the nearest directory at or above the current one with a `project.yaml`. A
  module file passed on the way that the project does not list makes its directory a single-module
  project.
- `project.yaml`: `modules` (plain paths and globs) and `plugins`.
- `module.yaml` and the templates it applies (`apply`, transitively): `product`, `description`,
  `dependencies` and `settings`, with `test-` and `@jvm` variants, read against the toolchain's
  schema (`src/schema`). Of `repositories`, only `mavenLocal` is used; `layout` and `tasks` are
  read but not yet used;
  `plugins`, `mavenPlugins`, `aliases` and `pluginInfo` are accepted without being read.

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

The library's public surface is the command boundary: `model` (read a project), `configuration`,
`dependencies`, `diagnostic` and `show`. Discovery, the file-system inventory, YAML,
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
