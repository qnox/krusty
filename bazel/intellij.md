# Building an intellij-community target with krusty

`krusty_jvm_library(use_worker = True)` speaks the same worker vocabulary as intellij-community's
`build/jvm-rules` `jvm-inc-builder`, so a `jvm_library` target can be moved onto krusty by changing
which rule it loads — not by rewriting its options.

Everything below was derived from that repository at `3f14adc599ef` and is reproduced here because
the mapping is the integration; running it needs a bazel this machine does not have.

## What the project's options become

`build/compiler-options.bzl` defines the defaults every target inherits. `kotlinc-options.bzl` turns
them into worker flags, and krusty's `--persistent_worker` parses exactly those:

| `create_kotlinc_options` | worker flag | krusty |
| --- | --- | --- |
| `jvm_default = "no-compatibility"` | `--jvm_default no-compatibility` | default methods, no `$DefaultImpls`, `jvmClassFlags` 1 |
| `x_no_param_assertions` | `--x_no_param_assertions` | no `Intrinsics.checkNotNullParameter` |
| `x_no_call_assertions` | `--x_no_call_assertions` | inert; krusty emits no call assertions |
| `x_lambdas = "indy"` | `--x_lambdas indy` | what krusty emits already |
| `x_sam_conversions = "indy"` | `--x_sam_conversions indy` | likewise |
| `jvm_target = "25"` | `--jvm_target 25` | `-jvm-target 25` (class file v69) |
| `api_version` / `language_version = "2.4"` | `--api_version` / `--language_version` | passed through |
| `progressive = True` | `--progressive` | `-progressive` |
| `x_x_language = ["+…"]` | `--x_xlanguage +…` | `-XXLanguage:+…` |
| `opt_in = [...]` | `--opt_in …` | `-opt-in=…` |

A value krusty cannot emit — `jvm_default = "disable"`, `x_lambdas = "class"` — fails the action
rather than producing a different artifact than the target asked for.

## Wiring

Add krusty to `MODULE.bazel` (or a local `WORKSPACE` override) and point the flag at a built binary:

```python
bazel_dep(name = "krusty", version = "0.0.1")
```

```bash
cargo build --release          # in the krusty checkout
bazel build //some:target \
  --//bazel:krusty_binary=//tools:krusty \
  --//bazel:use_krusty=true
```

Then, for a target you want krusty to build, swap the loaded symbol:

```python
# was: load("@rules_jvm//:jvm.bzl", "jvm_library")
load("@krusty//bazel:defs.bzl", krusty_jvm_library = "krusty_jvm_library")

krusty_jvm_library(
    name = "util",
    srcs = glob(["src/**/*.kt"]),
    deps = [...],
    module_name = "intellij.platform.util",
    jvm_target = "25",
    kotlinc_opts = ["-Xjvm-default=all", "-progressive"],
    use_worker = True,
)
```

## Which targets are eligible today

**Kotlin-only targets.** krusty has no Java front end, and `jvm-inc-builder` compiles both languages
in one action (`--java-count`), so a target with any `.java` source is refused by the worker — loudly,
with the target label, rather than by emitting a jar missing those classes.

That is a real restriction on this project: most `jvm_library` targets in intellij-community are
mixed. The parity scan (`scripts/ij-parity.py`) reports which modules are Kotlin-only.

**Targets using `associates`.** `--friends` grants a target visibility of another module's `internal`
declarations. krusty has no `-Xfriend-paths` and hides classpath `internal` members by design, so
such a target is refused rather than failing later with "unresolved" on every internal reference.

Note also that intellij's own `jvm-inc-builder` speaks length-prefixed PROTOBUF work requests, while
krusty's worker speaks line-delimited JSON (`requires-worker-protocol: json`). krusty is compatible
with the builder's ARGUMENT surface, not a drop-in replacement for the builder binary inside
intellij's own rule — which is why the target has to load `krusty_jvm_library` instead.

Three further gaps, each stated by the worker rather than discovered later:

* **No reduced ABI jar.** `--abi-out` gets a copy of the full jar, so a dependent rebuilds on any
  change instead of only on an ABI change. Correct, just less incremental.
* **No build-supplied compiler plugins.** A target with `--plugin-id` (the Compose plugin, the
  serialization plugin) is refused; krusty loads its own plugins, not bazel-supplied jars.
* **No resources.** krusty's jar holds class files and the `kotlin_module` index; a target passing
  `--resources` is refused rather than shipped a jar silently missing them.

Options that are understood but change nothing krusty emits are reported in the work response, which
bazel prints, rather than dropped in silence.
