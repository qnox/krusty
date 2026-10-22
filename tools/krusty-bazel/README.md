# krusty for Bazel

`krusty_jvm_library` compiles Kotlin sources with krusty and returns a `JavaInfo`, so ordinary
`java_*` and `kt_*` targets depend on it unchanged. A released module brings its own compiler: it
registers the release's prebuilt binaries as a toolchain per host, and Bazel downloads only the
one the execution platform needs.

## Using a release

Every krusty release is a version of the `krusty` module in the krusty Bazel registry,
`https://krustythecompiler.dev/bazel-registry`. Add it before the central registry in `.bazelrc`:

```
common --registry=https://krustythecompiler.dev/bazel-registry
common --registry=https://bcr.bazel.build
```

and depend on a release in `MODULE.bazel`:

```python
bazel_dep(name = "krusty", version = "<version>")
```

The registry is generated from the GitHub releases, each of which carries the module as
`krusty-bazel-<version>.tar.gz`. Without the registry, depend on that archive directly:

```python
bazel_dep(name = "krusty", version = "<version>")
archive_override(
    module_name = "krusty",
    urls = ["https://github.com/qnox/krusty/releases/download/v<version>/krusty-bazel-<version>.tar.gz"],
    integrity = "sha256-<base64 of the digest in krusty-bazel-<version>.tar.gz.sha256>",
)
```

Then load the rule:

```python
load("@krusty//:defs.bzl", "krusty_jvm_library")

krusty_jvm_library(
    name = "greet",
    srcs = ["src/Greet.kt"],
    deps = [":kotlin-stdlib"],
    kotlinc_opts = ["-no-stdlib"],
)
```

The module's `MODULE.bazel` names the SHA-256 of every compiler archive, so the compiler is pinned
by the module version alone. Hosts with a release: Linux, macOS and Windows, each on x86_64 and
aarch64.

`JAVA_HOME`, which gives krusty its platform classpath, comes from the execution platform's Java
runtime toolchain (`--tool_java_runtime_version` selects it). A target's `env` can still set it.
The compiler does not carry a Kotlin standard library, so a target passes `-no-stdlib` and takes
`kotlin-stdlib` from `deps`, as above.

## Choosing another compiler

`--@krusty//:krusty_binary=<label>` runs any executable target instead of the toolchain, such as a
wrapper around a `cargo build --release` binary (see `smoke/`). A root module can also pin the
compiler of another release, or download the archives from a mirror:

```python
krusty = use_extension("@krusty//:extensions.bzl", "krusty")
krusty.release(
    version = "<version>",
    sha256 = {"x86_64-unknown-linux-gnu": "<sha256 of krusty-<version>-x86_64-unknown-linux-gnu.tar.gz>"},
    url = "https://mirror.example/krusty/v{version}/{archive}",
)
```

The root module's tag replaces the module's own; hosts it leaves out get no toolchain.

## Working on the rule

This directory is the module root. A source checkout carries no release, so it registers no
toolchain and builds need `--@krusty//:krusty_binary`. `smoke/` is the smallest workspace that
builds against it, and `intellij.md` covers building intellij-community targets. The release job
packages the module with `scripts/package-bazel-module.sh`, and the pages workflow regenerates the
registry with `site/scripts/bazel-registry.mjs`; CI's `bazel` job runs
`scripts/bazel-smoke.sh`, which builds a consumer of that package with the toolchain and with an
override, in action and worker mode, with automatic execution groups on and off.
