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
- Project discovery: the nearest directory at or above the current one with a `project.yaml`. A
  module file passed on the way that the project does not list makes its directory a single-module
  project.
- `project.yaml`: `modules` (plain paths and globs) and `plugins`.
- `module.yaml` and the templates it applies (`apply`, transitively): `product`, `description`,
  `dependencies` and `settings`, with `test-` and `@jvm` variants, read against the toolchain's
  schema (`src/schema`). `repositories`, `layout` and `tasks` are read but not yet used;
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

The library's public surface is the command boundary: `model` (read a project), `diagnostic` and
`show`. Discovery, the file-system inventory, YAML and the build-file readers are private.

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
