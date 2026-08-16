# Building an intellij-community target with krusty

`krusty_jvm_library(use_worker = True)` speaks the same worker vocabulary as intellij-community's
`build/jvm-rules` `jvm-inc-builder`, so a `jvm_library` target can be moved onto krusty by changing
which rule it loads — not by rewriting its options.

Everything below was derived from that repository at `3f14adc599ef`, and the option table was
checked against the worker in this repository.

The Starlark rule itself has no automated coverage here: `tests/bazel_cli_contract_e2e.rs` drives the
worker protocol directly because a Bazel action cannot run from the test suite. The rule and worker
were exercised once manually with Bazel 8.4.2 (two dependent Kotlin targets through one reused
worker, consumed by a downstream Java target); that run is not reproducible from this checkout, so
treat the rule wiring below as unverified by CI.

## What the project's options become

`build/compiler-options.bzl` defines the defaults every target inherits. `kotlinc-options.bzl` turns
them into worker flags, and krusty's `--persistent_worker` parses exactly those:

| `create_kotlinc_options` | worker flag | krusty |
| --- | --- | --- |
| `jvm_default = "no-compatibility"` | `--jvm_default no-compatibility` | default methods, no `$DefaultImpls`, `jvmClassFlags` 1 |
| `jvm_default = "enable"` | `--jvm_default enable` | default methods, but the `$DefaultImpls` copy ONLY for members with default parameter values — no `access$…$jd` bridges and no class forwarders, so NOT byte-compatible with kotlinc |
| `jvm_default = "disable"` | `--jvm_default disable` | fully abstract interface, bodies on `$DefaultImpls` as receiver-first statics |
| `x_no_param_assertions` | `--x_no_param_assertions` | no `Intrinsics.checkNotNullParameter` |
| `x_no_call_assertions` | `--x_no_call_assertions` | inert; krusty emits no call assertions |
| `x_lambdas = "indy"` | `--x_lambdas indy` | what krusty emits already |
| `x_sam_conversions = "indy"` | `--x_sam_conversions indy` | likewise |
| `jvm_target = "25"` | `--jvm_target 25` | `-jvm-target 25` (class file v69) |
| `api_version` / `language_version = "2.4"` | `--api_version` / `--language_version` | accepted current mode and reported as a no-op; other versions are refused |
| `progressive = True` | `--progressive` | reported as a diagnostics-only no-op |
| `x_x_language = ["+…"]` | `--x_xlanguage +…` | supported language features are forwarded; the project's eager-accessibility check is reported as a no-op |
| `opt_in = [...]` | `--opt_in …` | reported as a no-op because krusty does not enforce opt-in requirements |

`no-compatibility` and `disable` are emitted at parity with kotlinc; `enable` compiles but is not,
per the row above. All three build, so no target is refused for its `jvm_default` value —
but an `enable` target's jar differs from what kotlinc would produce, and its interface metadata
still advertises a compatibility copy that is not in the jar, which a downstream kotlinc compilation
can link against. `tests/jvm_default_mode_e2e.rs` carries the differential for each mode.

A value krusty cannot emit fails the action rather than producing a different artifact than the
target asked for. Today that includes `x_lambdas` / `x_sam_conversions = "class"`, an
`api_version`/`language_version` other than 2.4, a `-XXLanguage` feature outside the modelled set,
`--warn` other than `off`, `--x_explicit_api` other than `disable`, `--src-jars`, and any unknown
worker option.

## Wiring

Add krusty to `MODULE.bazel` and point the flag at a built binary. krusty is not published to any
registry, so `bazel_dep` alone does not resolve — it needs a local override naming the checkout:

```python
bazel_dep(name = "krusty", version = "0.0.1")
local_path_override(module_name = "krusty", path = "/path/to/krusty")
```

`//tools:krusty` below is a target the CONSUMING repository defines to wrap the binary built by
`cargo` (see `bazel/BUILD.bazel` for the shape); it is not provided by krusty.

```bash
cargo build --release          # in the krusty checkout
bazel build //some:target \
  --@krusty//bazel:krusty_binary=//tools:krusty
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
    # Without JAVA_HOME krusty has no platform classpath and every `java.*` reference is
    # unresolved, so a real module fails without this.
    env = {"JAVA_HOME": "/path/to/jdk"},
)
```

## Which targets are eligible today

**Kotlin-only targets.** krusty has no Java front end, and `jvm-inc-builder` compiles both languages
in one action (`--java-count`), so a target with any `.java` source is refused by the worker rather
than emitting a jar missing those classes.

That is a real restriction on this project: most `jvm_library` targets in intellij-community are
mixed. Nothing in this repository reports which ones are Kotlin-only — the parity scan overlays Java
stubs so mixed modules are analyzed rather than separated. `scripts/bazel-worker-probe.py` derives a
Kotlin-only set from the project model when it selects what to build.

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

One caveat on the rule itself: `krusty_jvm_library` has an attribute only for `jvm_target`, so every
other row above has to be re-spelled as a kotlinc flag in `kotlinc_opts`. That is not always
equivalent to the builder's own spelling — `--opt_in kotlin.RequiresOptIn` is inert, while the same
thing written as `kotlinc_opts = ["-opt-in", "kotlin.RequiresOptIn"]` FAILS the action, because the
CLI does not model `-opt-in` and the worker refuses anything it ignores.
