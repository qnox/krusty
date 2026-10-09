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

- YAML is parsed by `saphyr-parser` (YAML 1.2). Anchors, aliases, `!!` tags and a second document
  are reported with the toolchain's messages. A YAML syntax error is reported with the parser's
  message where the toolchain's PSI parser would recover.
- Module globs follow `java.nio` `glob:` semantics (`sun.nio.fs.Globs`) after the toolchain's
  normalisation; `glob.rs` is checked against a corpus recorded from the JDK
  (`scripts/kotlin-toolchain/GlobOracle.java`, `tests/recorded/globs.tsv`). Matches are sorted.
- Every file-system read goes through `inventory.rs`: bounded, sorted, and never through a symbolic
  link. A link met on the way to a module, or inside a walked region, is an error naming it.
- A `modules` path is walked component by component as the file system walks it, so `a/x/../b` is
  unresolved when `a/x` does not exist, as it is for the toolchain.
- Diagnostics are `(severity, message, file, line, column)` in the toolchain's order and words.
  Project-file errors stop before module files are read; module-file errors stop before module names
  are compared.

## Deliberate refusals

krusty-toolchain refuses, with an error naming itself:

- `project.amper` and `module.amper` files;
- a `modules` entry with a leading `/` or `//` (the toolchain accepts `//path`);
- `mavenPlugins` in `project.yaml`;
- product types other than `jvm/app`, `jvm/lib` and `jvm/amper-plugin`, and a JVM product whose
  `platforms` is anything but `[jvm]`;
- a qualifier other than `@jvm` on `dependencies`/`settings`, and any qualifier on another property.

## Differential tests

`tests/recorded/projects/*.case` hold a project's files and what `kotlin show modules` reported for
them: every problem (file relative to the root, line, column, severity, message) and, when the
project is read without errors, the module table. `tests/project_cases.rs` materialises each case
and requires the same problems in the same order and a byte-identical table. A `--- krusty` section
records what krusty-toolchain reports instead where it deliberately differs; it must be a refusal in
krusty-toolchain's name, or the toolchain must reject the case too.

`tests/recorded/settings/*.case` hold what `kotlin show settings --all-modules` printed and
reported; `tests/settings_cases.rs` requires the same problems and the same settings, line for
line (trailing spaces aside: the line naming a module is padded to 1,500 columns, which a unit test
checks).

Re-record after changing a case or moving to another toolchain version:

```text
export JAVA_HOME=<JDK 25> LC_ALL=C.UTF-8 KOTLIN_CLI_NO_WELCOME_BANNER=1
python3 scripts/kotlin-toolchain/record_projects.py <path to the kotlin wrapper> \
  crates/krusty-toolchain/tests/recorded/projects/*.case
python3 scripts/kotlin-toolchain/record_projects.py --settings <path to the kotlin wrapper> \
  crates/krusty-toolchain/tests/recorded/settings/*.case
```
