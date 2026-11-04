# krusty conformance corpus

Each project is built from a pinned commit with its own Gradle build, once with kotlinc and
once with the krusty Gradle plugin, and the class files of every Kotlin/JVM compilation are
compared byte for byte. Written by the `oss corpus` workflow (`.github/workflows/oss-corpus.yml`
on master); `projects/` holds each project's latest result and `history.tsv` every run.

| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes | Measured |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| Exposed | 9,302 | 138,105 | 0 | ok | 0 / 43 | 0 / 8,100 (0.0%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| koin | 10,025 | 33,677 | 123 | ok | 5 / 19 | 96 / 3,705 (2.6%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| kotlinx.coroutines | 13,822 | 102,096 | 897 | ok | 0 / 34 | 0 / 9,482 (0.0%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| ktor | 14,561 | 259,026 | 85 | ok | 0 / 220 | 0 / 15,418 (0.0%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| leakcanary | 30,004 | 100,627 | 172 | ok | 6 / 34 | 23 / 1,798 (1.3%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| Maestro | 16,310 | 65,644 | 0 | ok | 1 / 19 | 27 / 2,156 (1.3%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| mirai | 14,794 | 156,078 | 559 | ok | 3 / 56 | 9 / 11,399 (0.1%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| moshi | 10,169 | 16,749 | 13,212 | ok | 5 / 12 | 45 / 512 (8.8%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| nowinandroid | 22,017 | 26,596 | 0 | ok | 17 / 58 | 157 / 1,110 (14.1%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| okhttp | 47,107 | 126,665 | 3,816 | ok | 9 / 38 | 42 / 1,483 (2.8%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| okio | 9,055 | 46,812 | 2,398 | failed (exit 1) | 1 / 6 | 12 / 343 (3.5%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| quarkdown | 16,345 | 87,771 | 23 | ok | 5 / 37 | 28 / 2,345 (1.2%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| Unciv | 11,520 | 139,231 | 1,184 | ok | 0 / 4 | 0 / 3,341 (0.0%) | [2026-11-04, krusty e075f8f](https://github.com/qnox/krusty/actions/runs/41795013119) |
| **Measured total** | | | | | **51 / 574** | **427 / 60,849 (0.7%)** | |

## Missed compilations

- **Exposed**
  - 42 × not run: a task it depends on failed
  - 1 × exposed-core/src/main/kotlin/org/jetbrains/exposed/v1/core/Alias.kt:108:20: error: argument type mismatch: actual type is 'Column<*>?', but 'Column<out Any>' was expected.
- **koin**
  - 9 × not run: a task it depends on failed
  - 1 × projects/core/benchmark/src/jvmMain/kotlin/org/koin/benchmark/HeavyStartupBenchmark.kt:44:13: error: overload resolution ambiguity between candidates:
  - 1 × projects/core/koin-core-coroutines/src/jvmTest/kotlin/org/koin/test/LazyModuleOperatorTest.kt:31:34: error: unresolved reference 'bind'.
  - 1 × projects/core/koin-core/src/commonTest/kotlin/org/koin/core/ClosedScopeAPI.kt:253:42: error: type mismatch: inferred type is T but Scope was expected
  - 1 × projects/core/koin-test/src/commonMain/kotlin/org/koin/test/check/CheckModules.kt:154:22: error: none of the following candidates is applicable:
  - 1 × projects/ktor/koin-ktor/src/commonMain/kotlin/org/koin/ktor/ext/ApplicationExt.kt:22:29: error: unresolved reference 'KOIN_ATTRIBUTE_KEY'.
- **kotlinx.coroutines**
  - 34 × left to kotlinc: compiler plugin kotlin-atomicfu-compiler-plugin-embeddable, argument -Xrender-internal-diagnostic-names, argument -Xno-receiver-assertions
- **ktor**
  - 220 × left to kotlinc: argument -Xrender-internal-diagnostic-names
- **leakcanary**
  - 24 × not run: a task it depends on failed
  - 2 × left to kotlinc: krusty does not implement everything it requests (freeCompilerArg '-P' conflicts with compiler plugin configuration; configure the structured Gradle input instead)
  - 1 × leakcanary/leakcanary-deobfuscation-gradle-plugin/src/main/java/com/squareup/leakcanary/deobfuscation/LeakCanaryLeakDeobfuscationPlugin.kt:50:57: error: argument type mismatch: actual type is 'Any', but 'Variant' was expected.
  - 1 × shark/shark-hprof/src/main/java/shark/HprofDeobfuscator.kt:1:1: error: internal error: checked FIR construction failed: Check(BodyCheckFailure { span: Some(Span { lo: 4167, hi: 4254 }), kind: UnpublishableType(Error) })
- **Maestro**
  - 14 × not run: a task it depends on failed
  - 1 × maestro-ai/src/main/java/maestro/ai/cloud/ApiClient.kt:1:1: error: krusty: this construct is not yet supported by the IR backend
  - 1 × maestro-ios-driver/src/main/kotlin/util/LocalSimulatorUtils.kt:239:50: error: unresolved reference 'Path'.
  - 1 × maestro-utils/src/test/kotlin/MaestroTimerTest.kt:19:9: error: unresolved reference 'assertEquals'.
  - 1 × maestro-web/src/main/kotlin/maestro/web/cdp/CdpClient.kt:1:1: error: krusty: JVM backend inline error: call arity mismatch
- **mirai**
  - 48 × not run: a task it depends on failed
  - 1 × ci-release-helper/src/CiHelper.kt:198:68: error: krusty: empty arrayOf() needs an explicit type (unsupported)
  - 1 × mirai-console/tools/compiler-common/src/diagnostics/MiraiConsoleErrorsRendering.kt:40:9: error: unresolved reference 'put'.
  - 1 × mirai-console/tools/gradle-plugin/src/main/kotlin/MiraiConsoleGradlePlugin.kt:49:24: error: unresolved reference 'configureDependencies'.
  - 1 × mirai-core-utils/src/commonMain/kotlin/AtomicInteger.kt:21:28: error: overload resolution ambiguity between candidates:
  - 1 × mirai-deps-test/test/CoreDependencyResolutionTest.kt:27:6: error: annotation argument is not a supported compile-time constant
- **moshi**
  - 5 × not run: a task it depends on failed
  - 1 × moshi-kotlin-codegen/src/main/java/com/squareup/moshi/kotlin/codegen/api/DelegateKey.kt:75:45: error: type mismatch: inferred type is Array<out Any> but Array<Any> was expected
  - 1 × moshi-kotlin/src/main/java/com/squareup/moshi/kotlin/reflect/KotlinJsonAdapterFactory.kt:1:1: error: internal error: FIR lowering failed: IncompleteAnnotationConstruction(TypeName("kotlin/Metadata"))
- **nowinandroid**
  - 30 × left to kotlinc: krusty does not implement everything it requests (krusty: 'androidx.compose.compiler.plugins.kotlin' is only partly implemented by krusty's built-in implementation; a module that uses what it does not cover yet fails to compile.)
  - 9 × not run: a task it depends on failed
  - 1 × core/data/src/main/kotlin/com/google/samples/apps/nowinandroid/core/data/repository/CompositeUserNewsResourceRepository.kt:1:1: error: krusty: JVM backend inline error: the inliner rejected a call its route planning gave it
  - 1 × core/datastore/src/test/kotlin/com/google/samples/apps/nowinandroid/core/datastore/IntToStringIdsMigrationTest.kt:1:1: error: internal error: module signatures were not finalized and no source diagnostic explains it
- **okhttp**
  - 24 × not run: a task it depends on failed
  - 4 × left to kotlinc: compiler plugin burst-kotlin-plugin
  - 1 × okhttp-tls/src/main/kotlin/okhttp3/tls/internal/der/Adapters.kt:38:68: error: unresolved reference 'readBoolean'.
- **okio**
  - 2 × left to kotlinc: compiler plugin burst-kotlin-plugin
  - 1 × okio/jvm/jmh/src/jmh/java/com/squareup/okio/benchmarks/BenchmarkUtils.kt:31:12: error: unresolved reference 'commonToUtf8String'.
  - 1 × krusty: error: -Xjdk-release=8 compiles against the JDK 8 API from ct.sym, which krusty does not implement; the selected JDK is 21
  - 1 × not run: a task it depends on failed
- **quarkdown**
  - 29 × not run: a task it depends on failed
  - 1 × quarkdown-core/src/commonMain/kotlin/com/quarkdown/core/ast/dsl/InlineAstBuilder.kt:87:59: error: argument type mismatch: actual type is 'String', but 'InlineAstBuilder' was expected.
  - 1 × quarkdown-native-library-processor/src/main/kotlin/com/quarkdown/processor/discovery/AnnotationExtractor.kt:38:16: error: argument type mismatch: actual type is 'String?', but 'String' was expected.
  - 1 × quarkdown-template/src/main/kotlin/com/quarkdown/template/TemplateProcessor.kt:1:1: error: internal error: module signatures were not finalized and no source diagnostic explains it
- **Unciv**
  - 4 × left to kotlinc: compiler plugin purityPlugin

## Build cost

Each build runs alone on its own runner. Compile time sums the compile tasks: kotlinc's Kotlin task, and
krusty's replacement, support check and Kotlin task (kotlinc's compile when a compilation is left to it).
Peak memory sums every process the build started, daemons included; the compiler's is the Kotlin daemon's
in the reference build and the krusty processes' in the krusty build. A warm-up build downloads everything
first, and both measured builds run offline from the same caches.

| Project | Compile time, compilations krusty built (kotlinc / krusty) | Compile time, all compilations | Compiler peak RSS | Build peak RSS | Build wall time | Build CPU time |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Exposed | 0.0 s / 0.0 s | 206.8 s / 5.7 s | 3,606 MB / 198 MB | 5,137 MB / 2,102 MB | 114.5 s / 44.3 s | 404.7 s / 124.0 s |
| koin | 27.9 s / 8.9 s (3.1×) | 113.7 s / 38.1 s | 1,514 MB / 475 MB | 2,937 MB / 2,060 MB | 66.0 s / 67.6 s | 229.3 s / 196.1 s |
| kotlinx.coroutines | 0.0 s / 0.0 s | 145.3 s / 144.8 s | 3,213 MB / 32 MB | 4,902 MB / 5,946 MB | 119.6 s / 124.3 s | 393.6 s / 412.2 s |
| ktor | 0.0 s / 0.0 s | 409.8 s / 421.7 s | 4,216 MB / 46 MB | 9,723 MB / 9,633 MB | 228.7 s / 239.0 s | 760.4 s / 786.8 s |
| leakcanary | 23.4 s / 5.2 s (4.5×) | 148.9 s / 13.5 s | 2,130 MB / 344 MB | 3,970 MB / 2,634 MB | 84.7 s / 40.5 s | 296.9 s / 126.3 s |
| Maestro | 12.1 s / 0.8 s (15.1×) | 123.0 s / 6.4 s | 1,841 MB / 165 MB | 3,365 MB / 1,501 MB | 108.0 s / 50.3 s | 324.5 s / 135.3 s |
| mirai | 29.5 s / 4.4 s (6.7×) | 308.9 s / 25.1 s | 4,747 MB / 372 MB | 6,203 MB / 3,058 MB | 196.9 s / 75.8 s | 668.7 s / 245.2 s |
| moshi | 24.9 s / 6.7 s (3.7×) | 49.6 s / 10.1 s | 1,357 MB / 168 MB | 2,665 MB / 1,986 MB | 65.4 s / 44.1 s | 215.7 s / 123.3 s |
| nowinandroid | 55.5 s / 24.6 s (2.3×) | 140.9 s / 92.4 s | 4,536 MB / 260 MB | 9,085 MB / 6,601 MB | 98.0 s / 103.5 s | 363.7 s / 355.5 s |
| okhttp | 21.5 s / 13.6 s (1.6×) | 134.8 s / 54.0 s | 3,482 MB / 382 MB | 5,921 MB / 4,889 MB | 141.4 s / 109.3 s | 470.1 s / 351.7 s |
| okio | 1.7 s / 0.7 s (2.4×) | 26.5 s / 22.9 s | 2,153 MB / 136 MB | 4,339 MB / 4,220 MB | 79.0 s / 84.1 s | 251.7 s / 267.7 s |
| quarkdown | 25.7 s / 5.6 s (4.6×) | 202.5 s / 21.1 s | 1,691 MB / 245 MB | 3,060 MB / 2,255 MB | 125.5 s / 78.0 s | 455.1 s / 239.8 s |
| Unciv | 0.0 s / 0.0 s | 130.7 s / 140.5 s | 3,378 MB / 38 MB | 4,205 MB / 5,287 MB | 132.4 s / 154.0 s | 429.6 s / 495.4 s |
| **Total** | **222.2 s / 70.5 s (3.2×)** | **2,141.4 s / 996.3 s** | | | **1,560.1 s / 1,214.8 s** | **5,264.0 s / 3,859.3 s** |

## krusty hotspots

Sampled call stacks of the krusty processes of every krusty build (11,615 samples at 99 Hz).
*Self* counts the samples in the function itself, *total* those with it anywhere on the stack.

Flame graphs: [Exposed](projects/Exposed/krusty-flamegraph.svg), [koin](projects/koin/krusty-flamegraph.svg), [kotlinx.coroutines](projects/kotlinx.coroutines/krusty-flamegraph.svg), [ktor](projects/ktor/krusty-flamegraph.svg), [leakcanary](projects/leakcanary/krusty-flamegraph.svg), [Maestro](projects/Maestro/krusty-flamegraph.svg), [mirai](projects/mirai/krusty-flamegraph.svg), [moshi](projects/moshi/krusty-flamegraph.svg), [nowinandroid](projects/nowinandroid/krusty-flamegraph.svg), [okhttp](projects/okhttp/krusty-flamegraph.svg), [okio](projects/okio/krusty-flamegraph.svg), [quarkdown](projects/quarkdown/krusty-flamegraph.svg), [Unciv](projects/Unciv/krusty-flamegraph.svg).

| Function | Self | Total | Projects |
| --- | ---: | ---: | --- |
| `[libc.so.6]` | 5.1% | 98.6% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_theap_malloc_zero_aligned_at` | 3.6% | 13.8% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_free` | 2.9% | 12.1% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_block_nextx` | 2.8% | 2.8% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_free_block_local` | 1.8% | 3.5% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `zlib_rs::inflate::inflate_fast_help_avx2` | 1.8% | 2.4% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_unchecked_ptr_page` | 1.7% | 3.9% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_theap_get_free_small_page` | 1.6% | 1.9% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `<krusty::name_tree::Table>::probe` | 1.5% | 1.6% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_page_malloc_zero` | 1.4% | 4.4% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_page_map_at` | 1.4% | 1.4% | Exposed, Maestro, koin, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, quarkdown |
| `do_syscall_64` | 1.4% | 7.1% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `<krusty::name_tree::NameTree>::child_or_insert` | 1.1% | 2.5% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `<core::alloc::layout::Layout>::from_size_alignment_unchecked` | 1.1% | 1.1% | Exposed, Maestro, koin, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, quarkdown |
| `__rustc::__rust_dealloc` | 1.0% | 13.2% | Exposed, Maestro, koin, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_page_map_index` | 1.0% | 1.0% | Exposed, Maestro, koin, kotlinx.coroutines, leakcanary, mirai, moshi, nowinandroid, okhttp, quarkdown |
| `mi_block_set_nextx` | 1.0% | 1.9% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `kernel_init_pages` | 0.9% | 0.9% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, quarkdown |
| `<ahash::fallback_hash::AHasher as core::hash::Hasher>::write_u64` | 0.8% | 0.8% | Exposed, Maestro, koin, mirai, moshi, nowinandroid, okhttp, quarkdown |
| `core::slice::memchr::memchr_aligned` | 0.8% | 0.8% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |

## Gradle plugins

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `org.jetbrains.kotlin.plugin.scripting` | 13 | 348 |
| `org.jetbrains.kotlin.jvm` | 13 | 180 |
| `org.jetbrains.dokka` | 8 | 226 |
| `org.jetbrains.kotlin.multiplatform` | 8 | 168 |
| `org.jetbrains.kotlin.plugin.serialization` | 8 | 104 |
| `com.vanniktech.maven.publish.base` | 7 | 212 |
| `com.android.internal.version-check` | 7 | 104 |
| `com.vanniktech.maven.publish` | 5 | 189 |
| `com.android.internal.library` | 5 | 65 |
| `com.google.devtools.ksp` | 5 | 33 |
| `org.jetbrains.kotlinx.binary-compatibility-validator` | 5 | 27 |
| `io.gitlab.arturbosch.detekt` | 4 | 79 |
| `com.diffplug.spotless` | 4 | 72 |
| `com.android.internal.application` | 4 | 7 |
| `com.google.osdetector` | 4 | 4 |
| `org.jetbrains.kotlinx.atomicfu` | 3 | 151 |
| `org.jetbrains.kotlinx.kover` | 3 | 43 |
| `org.jetbrains.kotlin.plugin.compose` | 3 | 22 |
| `ru.vyarus.animalsniffer` | 2 | 34 |
| `com.android.internal.lint` | 2 | 30 |
| `com.gradleup.tapmoc` | 2 | 28 |
| `com.google.dagger.hilt.android` | 2 | 23 |
| `app.cash.burst` | 2 | 7 |
| `org.jetbrains.compose` | 2 | 5 |
| `org.jetbrains.compose.hot-reload` | 2 | 5 |
| `com.github.johnrengelman.shadow` | 2 | 3 |
| `com.android.internal.kotlin.multiplatform.library` | 2 | 2 |
| `com.google.protobuf` | 2 | 2 |
| `com.gradle.plugin-publish` | 2 | 2 |
| `me.champeau.jmh` | 2 | 2 |

## Kotlin compiler plugins

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `kotlin-serialization-compiler-plugin-embeddable` | 8 | 102 |
| `symbol-processing-api` | 3 | 8 |
| `kotlin-compose-compiler-plugin-embeddable` | 2 | 15 |
| `burst-kotlin-plugin` | 2 | 5 |
| `kotlin-atomicfu-compiler-plugin-embeddable` | 1 | 18 |
| `purity-compiler-plugin` | 1 | 5 |
| `kotlin-allopen-compiler-plugin-embeddable` | 1 | 1 |

## Compiler plugin arguments

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `-P plugin:androidx.compose.compiler.plugins.kotlin` | 1 | 1 |
