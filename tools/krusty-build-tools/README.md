# krusty Build Tools API implementation

A [Kotlin Build Tools API](https://github.com/JetBrains/kotlin/tree/master/compiler/build-tools)
implementation that compiles Kotlin/JVM with the krusty binary. A build tool that loads the
Build Tools API implementation from a classpath, such as JetBrains' Kotlin Toolchain, compiles
with krusty when this jar takes the place of `org.jetbrains.kotlin:kotlin-build-tools-impl`.

- One jar stands in for one Kotlin release (`-PkotlinVersion`, 2.4.20 by default): the build tool
  picks the implementation by its configured Kotlin version, and krusty compiles as that release
  (`-Xkotlin-reference-version`).
- Compiler arguments are forwarded as kotlinc command-line strings, which krusty parses itself.
- Only the JVM toolchain and in-process execution exist. Each compilation is one krusty process
  over the whole source set; the destination is cleared first, so incremental-compilation
  settings are accepted and do not change the result. Classpath snapshots are empty.
- The krusty binary comes from `KRUSTY_BIN` or the `krusty.binary` system property.

## Build and test

```sh
./gradlew jar                                    # build/libs/krusty-build-tools-<v>-kotlin-<kotlin>.jar
./gradlew test -Pkrusty.binary=/path/to/krusty   # drives a real krusty binary
```

## Kotlin Toolchain

The Kotlin Toolchain resolves the implementation by Maven coordinates and has no setting that
names another one. `install-into-kotlin-cache.sh` puts the jar into a toolchain shared cache as
that artifact; use a cache directory of its own, since every project built with that cache and
Kotlin version then compiles with krusty:

```sh
./install-into-kotlin-cache.sh build/libs/krusty-build-tools-0.0.1-kotlin-2.4.20.jar ~/.cache/krusty-kotlin
KOTLIN_SHARED_CACHE_DIR=~/.cache/krusty-kotlin KRUSTY_BIN=/path/to/krusty ./kotlin build
```

A project's own build directory also caches which implementation jar it resolved, so build a
project with a fresh build directory when switching it between kotlinc and krusty.
