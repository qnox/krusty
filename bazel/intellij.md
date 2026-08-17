# Building an intellij-community target with krusty

`krusty_jvm_library(use_worker = True)` speaks the same worker vocabulary as intellij-community's
`build/jvm-rules` `jvm-inc-builder`, so a `jvm_library` target can be moved onto krusty by changing
which rule it loads — not by rewriting its options.

Everything below was derived from that repository at `3f14adc599ef`, and the option table was
checked against the worker in this repository.

The Starlark rule has no automated coverage — `tests/bazel_cli_contract_e2e.rs` drives the worker
protocol directly, because a Bazel action cannot run from the test suite. What it has instead is
`bazel/smoke/`, a workspace small enough to read that compiles a Kotlin target with
`krusty_jvm_library`; running it produces `bazel-bin/greet.jar` holding `smoke/Greeter.class`,
`META-INF/smoke.kotlin_module` and a manifest, with `1 worker` in the build's process summary (the
`KrustyCompile` mnemonic itself shows only under `--subcommands`, `bazel aquery`, or on failure).
Verified against Bazel 9 and 8.4.2. It is not wired into CI, and it needs two machine paths filled
in before it runs, so it is a recipe you follow — not a gate, and not proof that a target as large
as an intellij module builds.

## What the project's options become

`build/compiler-options.bzl` defines the defaults every target inherits. `kotlinc-options.bzl` turns
them into worker flags, and krusty's `--persistent_worker` parses exactly those:

| `create_kotlinc_options` | worker flag | krusty |
| --- | --- | --- |
| `jvm_default = "no-compatibility"` | `--jvm_default no-compatibility` | default methods, no `$DefaultImpls`, `jvmClassFlags` 1 |
| `jvm_default = "enable"` | `--jvm_default enable` | default methods plus the full compatibility surface: `access$…$jd` bridges on the interface, a `$DefaultImpls` holder whose statics forward to those bridges (`@Deprecated`, byte-tested against kotlinc), and `invokespecial` forwarder overrides on implementing classes |
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
`cargo` (see `bazel/BUILD.bazel` for the shape, and `bazel/smoke/tools/krusty/` for a working one);
it is not provided by krusty. Two constraints that wrapper must respect, both found by running it:
Bazel gives an action no environment beyond what the rule passes — so a `set -u` script building its
path from `$HOME` dies with "HOME: unbound variable", and this is not sandbox-specific
(`--spawn_strategy=local` behaves the same); and under Bazel 9 `sh_binary` is no longer native and
must be loaded from `rules_shell`.

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
    # krusty resolves the stdlib by walking up from its own binary to `kotlin-versions` and looking
    # for `target/cache/kotlinc/<ver>` beneath it. (Its $HOME/.gradle and $HOME/.m2 fallbacks are
    # unreachable under Bazel, which passes no HOME. `KRUSTY_KOTLINC` in `env` is another way out.)
    # A binary shipped without one — a fresh worktree, a copied release artifact — fails with
    # "cannot locate kotlin-stdlib.jar; configure a Kotlin distribution or pass -no-stdlib".
    # `-no-stdlib` opts out and takes the stdlib from `deps` instead.
    kotlinc_opts = ["-no-stdlib", "-Xjvm-default=all", "-progressive"],
    use_worker = True,
    # Without JAVA_HOME krusty has no platform classpath and every `java.*` reference is
    # unresolved, so a real module fails without this. It must be set HERE: the rule passes `env`
    # to `ctx.actions.run`, which REPLACES the action environment — `--action_env` is not merged.
    # Measured, an action sees only what this dict carries plus TMPDIR; notably no HOME and no PATH,
    # so anything else the compiler needs (KRUSTY_KOTLINC, KRUSTY_TRACE) must be listed here too —
    # including KRUSTY_BINARY if, like `bazel/smoke/tools/krusty/`, the wrapper reads it.
    env = {
        "JAVA_HOME": "/path/to/jdk",
        "KRUSTY_BINARY": "/path/to/krusty/target/release/krusty",
    },
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
## The macOS SDK repository (building on a Mac without JetBrains credentials)

intellij-community's C/C++ toolchain (the `llvm` Bazel module, exercised even by pure-JVM builds
because the toolchain registers globally) fetches an Apple Command Line Tools SDK package as the
`@@llvm++osx+macos_sdk` repository. On a machine where that download is not reachable (the fetch
goes through JetBrains infrastructure that answers 401 without credentials), any
`bazel build` in the repository fails during repository fetching — before a single krusty action
runs.

The workaround is a local repository override pointing at the SDK already installed with Xcode's
Command Line Tools. Create this layout anywhere (`/tmp/macos_sdk_override` here):

```
macos_sdk_override/
  REPO.bazel            # marker; a comment is enough
  BUILD.bazel           # empty
  sysroot/
    BUILD.bazel         # headers_directory target, see below
    usr/
      include -> /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include
      lib     -> /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/lib
```

`sysroot/BUILD.bazel`:

```python
load("@llvm//:directory.bzl", "headers_directory")

headers_directory(
    name = "sysroot",
    path = ".",
    visibility = ["//visibility:public"],
)
```

Then add to every `bazel` invocation:

```
--override_repository=llvm++osx+macos_sdk=/tmp/macos_sdk_override
```

Two constraints, both found by running it:

* **Expose ONLY `usr/include` and `usr/lib`, as symlinks.** Symlinking the whole SDK breaks
  Bazel's globbing twice: `Ruby.framework` contains a self-referential symlink the glob walker
  follows forever, and man-page filenames containing `:` are invalid as Bazel labels.
* The `headers_directory` rule comes from the `llvm` module itself, so the override only works
  inside a repository that already depends on `llvm` (intellij-community does).

Setup on a new machine:

```bash
mkdir -p /tmp/macos_sdk_override/sysroot/usr
printf '# Local stand-in for the JetBrains-hosted macOS SDK package, which requires credentials.\n' \
  > /tmp/macos_sdk_override/REPO.bazel
touch /tmp/macos_sdk_override/BUILD.bazel
cat > /tmp/macos_sdk_override/sysroot/BUILD.bazel <<'EOF'
load("@llvm//:directory.bzl", "headers_directory")

headers_directory(
    name = "sysroot",
    path = ".",
    visibility = ["//visibility:public"],
)
EOF
ln -s /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include /tmp/macos_sdk_override/sysroot/usr/include
ln -s /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/lib /tmp/macos_sdk_override/sysroot/usr/lib
```

With the override in place, the icons-api target builds with:

```bash
JAVA_HOME=/path/to/jdk21 \
bazel build //platform/icons-api:icons-api_krusty \
  --override_repository=llvm++osx+macos_sdk=/tmp/macos_sdk_override \
  --@krusty//bazel:krusty_binary=//tools/krusty:krusty
```
