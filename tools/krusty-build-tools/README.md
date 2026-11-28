# krusty Build Tools API implementation

A [Kotlin Build Tools API](https://github.com/JetBrains/kotlin/tree/master/compiler/build-tools)
implementation that compiles Kotlin/JVM with the krusty binary. A build tool that loads the
Build Tools API implementation from a classpath, such as JetBrains' Kotlin Toolchain, compiles
with krusty when this jar takes the place of `org.jetbrains.kotlin:kotlin-build-tools-impl`.

- One jar stands in for one Kotlin release (`-PkotlinVersion`, 2.4.21 by default): the build tool
  picks the implementation by its configured Kotlin version, and krusty compiles as that release
  (`-Xkotlin-reference-version`).
- Compiler arguments are forwarded as kotlinc command-line strings, which krusty parses itself.
- Only the JVM toolchain exists. The in-process and daemon execution policies compile alike: the
  daemon policy asks for a compile outside the build tool's process, which is where krusty always
  compiles, and its options configure a Kotlin daemon JVM krusty never starts. Each compilation is
  one krusty process over the whole source set; the destination is cleared first. With an incremental-compilation
  configuration the compilation is skipped instead when its inputs match the last successful run
  in that configuration's working directory and the output is untouched. `krusty
  compilation-fingerprint` digests those inputs by contents: everything the compilation reads,
  with a class directory taken as what a dependent observes, so an upstream body-only edit does
  not recompile dependents. Classpath snapshots are empty.
- The krusty binary comes from `KRUSTY_BIN` or the `krusty.binary` system property.

## Build and test

```sh
./gradlew jar                                    # build/libs/krusty-build-tools-<v>-kotlin-<kotlin>.jar
./gradlew check -Pkrusty.binary=/path/to/krusty   # drives a real krusty binary
```

`check` also builds a Kotlin/JVM project with the Kotlin Gradle plugin through this jar
(`kotlinGradlePluginTest`, a Gradle TestKit build).

CI runs `scripts/package-build-tools.sh` once per supported Kotlin version (`kotlin-versions`),
against the krusty binary built for that commit. It tests, builds the jar twice from clean to
check that the bytes are reproducible, and uploads the jar, the cache installer and their
`SHA256SUMS` as a CI artifact. A release publishes those artifacts as they are.

## Kotlin Gradle plugin

Since Kotlin 2.4 the Kotlin Gradle plugin compiles Kotlin/JVM through the Build Tools API, and it
loads the implementation from its `kotlinBuildToolsApiClasspath` configuration. The plugin adds
`kotlin-build-tools-impl` and `kotlin-build-tools-compat` to that configuration itself, and the
compatibility layer carries an implementation of its own, so exclude both and add this jar:

```kotlin
configurations.kotlinBuildToolsApiClasspath {
    exclude(group = "org.jetbrains.kotlin", module = "kotlin-build-tools-impl")
    exclude(group = "org.jetbrains.kotlin", module = "kotlin-build-tools-compat")
}
dependencies {
    kotlinBuildToolsApiClasspath(files("krusty-build-tools-0.0.1-kotlin-2.4.21.jar"))
}
```

Run Gradle with `KRUSTY_BIN` set to the krusty binary. The jar's Kotlin release must be the
plugin's. The plugin passes every source twice (in its arguments and as the operation's sources)
and `-Xallow-no-source-files`; krusty compiles each source once, as kotlinc does.

## Kotlin Toolchain

The Kotlin Toolchain resolves the implementation by Maven coordinates and has no setting that
names another one. `install-into-kotlin-cache.sh` puts the jar into a toolchain shared cache as
that artifact; use a cache directory of its own, since every project built with that cache and
Kotlin version then compiles with krusty:

```sh
./install-into-kotlin-cache.sh build/libs/krusty-build-tools-0.0.1-kotlin-2.4.21.jar ~/.cache/krusty-kotlin
KOTLIN_SHARED_CACHE_DIR=~/.cache/krusty-kotlin KRUSTY_BIN=/path/to/krusty ./kotlin build
```

A project's own build directory also caches which implementation jar it resolved, so build a
project with a fresh build directory when switching it between kotlinc and krusty.
