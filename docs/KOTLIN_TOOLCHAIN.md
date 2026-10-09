# Kotlin Toolchain projects

`crates/krusty-toolchain` reads projects in the Kotlin Toolchain format (`project.yaml`,
`module.yaml`), the format JetBrains' `kotlin` command (formerly Amper) builds. The goal is a
drop-in replacement for that command that compiles with krusty, the way krusty replaces `kotlinc`.
The reference is Kotlin Toolchain 0.13.0.

## What is implemented

- `krusty-toolchain [--project-dir=<path>] show modules [--format=plain|table]` prints exactly what
  `kotlin show modules` prints.
- Project discovery: the nearest directory at or above the current one with a `project.yaml`. A
  module file passed on the way that the project does not list makes its directory a single-module
  project.
- `project.yaml`: `modules` (plain paths and globs) and `plugins`.
- `module.yaml`: `product` and `description`. The other module properties are recognised (so a
  misspelt one is reported) but not yet read.

## Reading rules

- YAML is parsed by `saphyr-parser` (YAML 1.2). Anchors, aliases, `!!` tags, custom `!` tags
  (except on a file's top mapping) and a second document are reported with the toolchain's
  messages. A YAML syntax error is reported with the parser's
  message where the toolchain's PSI parser would recover.
- Module globs follow `java.nio` `glob:` semantics (`sun.nio.fs.Globs`) after the toolchain's
  normalisation; `glob.rs` is checked against a corpus recorded from the JDK
  (`scripts/kotlin-toolchain/GlobOracle.java`, `tests/recorded/globs.tsv`). Matches are sorted.
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

`tests/recorded/projects/*.case` hold a project's files and what `kotlin show modules` reported for
them: every problem (file relative to the root, line, column, severity, message) and, when the
project is read without errors, the module table. `tests/project_cases.rs` materialises each case
and requires the same problems in the same order and a byte-identical table. A `--- krusty` section
records what krusty-toolchain reports instead where it deliberately differs; it must be a refusal in
krusty-toolchain's name, or the toolchain must reject the case too.

Re-record after changing a case or moving to another toolchain version:

```text
export JAVA_HOME=<JDK 25> LC_ALL=C.UTF-8 KOTLIN_CLI_NO_WELCOME_BANNER=1
python3 scripts/kotlin-toolchain/record_projects.py scripts/kotlin-toolchain/kotlin \
  crates/krusty-toolchain/tests/recorded/projects/*.case
```

`scripts/kotlin-toolchain/kotlin` is the toolchain's own wrapper, pinning the version and checksum
of the distribution it runs. The committed recordings keep these tests fast and offline; the
`kotlin-toolchain` CI job (a gate of `ci-and-conformance`) keeps them true by running
`scripts/kotlin-toolchain/verify_recordings.sh`, which re-records every project case through that
wrapper and the glob corpus through `GlobOracle.java` on a JDK 25, and fails on any difference.
