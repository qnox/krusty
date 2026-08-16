# krusty rule smoke test

The smallest workspace that compiles Kotlin with `krusty_jvm_library`, so the rule's behaviour under
Bazel can be checked by running it rather than taken on trust.

## Running it

All paths below are relative to this directory (`bazel/smoke`).

1. Build the compiler, and provision the Kotlin distribution the jar below comes from:

       (cd ../.. && cargo build --release && just kotlinc)

   `just kotlinc` populates `target/cache/kotlinc/<ver>/`, where `<ver>` is `just max-version`
   (2.4.10 at the time of writing) — or just look at `ls ../../target/cache/kotlinc/`.

2. Copy the stdlib in (`lib/*.jar` is gitignored; the directory is committed):

       cp ../../target/cache/kotlinc/2.4.10/kotlinc/lib/kotlin-stdlib.jar lib/

3. **Edit the two paths in `BUILD.bazel`** — `JAVA_HOME` and `KRUSTY_BINARY` are passed through the
   rule's `env` attribute, which REPLACES the action environment, so they cannot be supplied with
   `--action_env` from the command line.

4. Build:

       bazel build //:greet --@krusty//bazel:krusty_binary=//tools/krusty:krusty

   Under Bazel 9 this prints a `rules_java` version-skew warning (the fixture pins 8.14.0, Bazel 9
   resolves 9.1.0). It is noise, not a failure.

`bazel-bin/greet.jar` then holds `smoke/Greeter.class`, `META-INF/smoke.kotlin_module` and a
manifest, and the build log reports one `KrustyCompile` worker.

## What running it actually establishes

* The rule drives krusty as a Bazel persistent worker and produces a loadable jar. Verified on
  Bazel 9 and 8.4.2.
* `JAVA_HOME` must reach the action: `src/Greet.kt` uses `java.util.ArrayList` ON PURPOSE, so a
  wrong path fails the build instead of passing quietly.
* Bazel gives an action no environment beyond what the rule passes — no `HOME`, no `PATH`. A
  wrapper script that builds its path from `$HOME` under `set -u` dies with "HOME: unbound
  variable"; this is not sandbox-specific, `--spawn_strategy=local` behaves the same.
* Under Bazel 9 `sh_binary` is not a native rule and must be loaded from `rules_shell`.

What it does NOT establish: that `-no-stdlib` is required. Follow the recipe above and it is not —
step 1 provisions the very distribution krusty would find on its own, and the build is green with
the flag deleted. It is in `BUILD.bazel` so the fixture still works when `KRUSTY_BINARY` points at
a binary from somewhere else (a fresh worktree, a copied release artifact), which is the case that
first surfaced it. Drop it if you would rather exercise the default stdlib path.
