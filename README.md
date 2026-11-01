# krusty conformance corpus

Each project is built from a pinned commit with its own Gradle build, once with kotlinc and
once with the krusty Gradle plugin, and the class files of every Kotlin/JVM compilation are
compared byte for byte. Written by the `oss corpus` workflow (`.github/workflows/oss-corpus.yml`
on master); `projects/` holds each project's latest result and `history.tsv` every run.

| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes | Measured |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| Exposed | 9,302 | 138,105 | 0 | ok | 0 / 43 | 0 / 8,100 (0.0%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| koin | 10,025 | 33,677 | 123 | ok | 5 / 19 | 95 / 3,705 (2.6%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| kotlinx.coroutines | 13,823 | 102,096 | 897 | ok | 0 / 34 | 0 / 9,482 (0.0%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| ktor | 14,561 | 259,026 | 85 | ok | 0 / 220 | 0 / 15,418 (0.0%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| leakcanary | 30,003 | 100,627 | 172 | ok | 6 / 34 | 23 / 1,798 (1.3%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| Maestro | 16,282 | 65,644 | 0 | ok | 1 / 19 | 27 / 2,156 (1.3%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| mirai | 14,795 | 156,078 | 559 | ok | 3 / 56 | 8 / 11,399 (0.1%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| moshi | 10,168 | 16,749 | 13,212 | ok | 5 / 12 | 45 / 512 (8.8%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| nowinandroid | 21,991 | 26,596 | 0 | ok | 17 / 58 | 156 / 1,110 (14.1%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| okhttp | 47,106 | 126,665 | 3,816 | ok | 9 / 38 | 40 / 1,483 (2.7%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| okio | 9,055 | 46,812 | 2,398 | failed (exit 1) | 1 / 6 | 12 / 343 (3.5%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| quarkdown | 16,344 | 87,771 | 23 | ok | 5 / 37 | 28 / 2,345 (1.2%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| Unciv | 11,516 | 139,231 | 1,184 | ok | 0 / 4 | 0 / 3,341 (0.0%) | [2026-11-01, krusty 1fd3141](https://github.com/qnox/krusty/actions/runs/41286577483) |
| **Measured total** | | | | | **51 / 574** | **422 / 60,849 (0.7%)** | |

## Missed compilations

- **Exposed**
  - 42 × not run: a task it depends on failed
  - 1 × exposed-core/src/main/kotlin/org/jetbrains/exposed/v1/core/Alias.kt:108:20: error: argument type mismatch: actual type is 'Column<*>?', but 'Column<out Any>' was expected.
- **koin**
  - 9 × not run: a task it depends on failed
  - 1 × projects/core/benchmark/src/jvmMain/kotlin/org/koin/benchmark/HeavyStartupBenchmark.kt:44:13: error: overload resolution ambiguity
  - 1 × projects/core/koin-core-coroutines/src/jvmTest/kotlin/org/koin/test/LazyModuleOperatorTest.kt:1:1: error: krusty: JVM backend inline error: the inliner rejected a call its route planning gave it
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
  - 1 × moshi-kotlin/src/main/java/com/squareup/moshi/kotlin/reflect/IndexedParameterMap.kt:19:3: error: 'containsKey' overrides nothing. Potential signatures for overriding:
- **nowinandroid**
  - 30 × left to kotlinc: krusty does not implement everything it requests (krusty: 'androidx.compose.compiler.plugins.kotlin' is only partly implemented by krusty's built-in implementation; a module that uses what it does not cover yet fails to compile.)
  - 9 × not run: a task it depends on failed
  - 1 × core/data/src/main/kotlin/com/google/samples/apps/nowinandroid/core/data/repository/DefaultSearchContentsRepository.kt:1:1: error: the compiled inline body of `flatMapLatest` cannot be inlined: Regeneration(Unsupported("a $$forInline method without a continuation"))
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
  - 1 × quarkdown-core/src/commonMain/kotlin/com/quarkdown/core/ast/base/inline/ReferenceFootnote.kt:46:68: error: argument type mismatch: actual type is 'List<Any>', but 'List<Node>' was expected.
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
| Exposed | 0.0 s / 0.0 s | 283.9 s / 8.1 s | 3,458 MB / 197 MB | 4,970 MB / 2,113 MB | 156.7 s / 58.8 s | 557.3 s / 166.7 s |
| koin | 27.7 s / 8.1 s (3.4×) | 110.4 s / 34.1 s | 1,556 MB / 448 MB | 2,813 MB / 2,169 MB | 63.9 s / 64.2 s | 220.3 s / 187.6 s |
| kotlinx.coroutines | 0.0 s / 0.0 s | 182.9 s / 182.8 s | 3,808 MB / 33 MB | 5,628 MB / 5,330 MB | 150.3 s / 158.9 s | 501.7 s / 528.0 s |
| ktor | 0.0 s / 0.0 s | 648.8 s / 668.2 s | 4,423 MB / 54 MB | 9,581 MB / 9,611 MB | 389.6 s / 385.6 s | 1,297.5 s / 1,274.5 s |
| leakcanary | 29.1 s / 6.0 s (4.9×) | 182.3 s / 16.4 s | 2,069 MB / 385 MB | 3,938 MB / 2,653 MB | 104.0 s / 49.0 s | 366.2 s / 153.2 s |
| Maestro | 7.7 s / 0.6 s (12.8×) | 86.5 s / 5.2 s | 1,893 MB / 172 MB | 3,427 MB / 1,435 MB | 77.0 s / 40.6 s | 225.6 s / 107.7 s |
| mirai | 18.7 s / 3.9 s (4.8×) | 239.1 s / 21.1 s | 4,769 MB / 475 MB | 6,334 MB / 2,997 MB | 150.6 s / 58.5 s | 509.0 s / 187.7 s |
| moshi | 23.2 s / 6.5 s (3.6×) | 46.3 s / 9.6 s | 1,348 MB / 170 MB | 2,712 MB / 2,182 MB | 62.4 s / 42.6 s | 207.2 s / 118.7 s |
| nowinandroid | 79.9 s / 40.2 s (2.0×) | 203.3 s / 137.7 s | 3,818 MB / 310 MB | 7,999 MB / 6,424 MB | 138.5 s / 141.6 s | 513.2 s / 494.0 s |
| okhttp | 15.2 s / 9.9 s (1.5×) | 91.5 s / 39.5 s | 3,316 MB / 383 MB | 5,977 MB / 5,592 MB | 92.4 s / 77.9 s | 306.7 s / 247.5 s |
| okio | 2.2 s / 0.8 s (2.8×) | 34.3 s / 30.5 s | 2,364 MB / 138 MB | 4,163 MB / 4,291 MB | 101.8 s / 111.2 s | 323.7 s / 354.4 s |
| quarkdown | 17.7 s / 2.9 s (6.1×) | 138.1 s / 13.1 s | 1,704 MB / 255 MB | 3,077 MB / 2,226 MB | 84.2 s / 52.3 s | 304.0 s / 160.4 s |
| Unciv | 0.0 s / 0.0 s | 128.7 s / 137.2 s | 3,299 MB / 46 MB | 4,152 MB / 5,101 MB | 131.4 s / 150.5 s | 420.8 s / 485.7 s |
| **Total** | **221.4 s / 78.9 s (2.8×)** | **2,376.1 s / 1,303.5 s** | | | **1,702.8 s / 1,391.7 s** | **5,753.2 s / 4,466.1 s** |

## krusty hotspots

Sampled call stacks of the krusty processes of every krusty build (11,427 samples at 99 Hz).
*Self* counts the samples in the function itself, *total* those with it anywhere on the stack.

Flame graphs: [Exposed](projects/Exposed/krusty-flamegraph.svg), [koin](projects/koin/krusty-flamegraph.svg), [kotlinx.coroutines](projects/kotlinx.coroutines/krusty-flamegraph.svg), [ktor](projects/ktor/krusty-flamegraph.svg), [leakcanary](projects/leakcanary/krusty-flamegraph.svg), [Maestro](projects/Maestro/krusty-flamegraph.svg), [mirai](projects/mirai/krusty-flamegraph.svg), [moshi](projects/moshi/krusty-flamegraph.svg), [nowinandroid](projects/nowinandroid/krusty-flamegraph.svg), [okhttp](projects/okhttp/krusty-flamegraph.svg), [okio](projects/okio/krusty-flamegraph.svg), [quarkdown](projects/quarkdown/krusty-flamegraph.svg), [Unciv](projects/Unciv/krusty-flamegraph.svg).

| Function | Self | Total | Projects |
| --- | ---: | ---: | --- |
| `[libc.so.6]` | 5.3% | 98.6% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_theap_malloc_zero_aligned_at` | 3.6% | 13.1% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_free` | 3.1% | 11.8% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_block_nextx` | 2.1% | 2.1% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `do_syscall_64` | 2.1% | 9.2% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `zlib_rs::inflate::inflate_fast_help_avx2` | 1.8% | 2.3% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `<krusty::name_tree::Table>::probe` | 1.8% | 1.8% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_theap_get_free_small_page` | 1.7% | 2.1% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_unchecked_ptr_page` | 1.7% | 3.7% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_free_block_local` | 1.5% | 3.2% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_page_map_at` | 1.3% | 1.3% | Exposed, Maestro, koin, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `__rustc::__rust_dealloc` | 1.1% | 12.9% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `<krusty::name_tree::NameTree>::child_or_insert` | 1.1% | 2.7% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_block_set_nextx` | 1.0% | 1.7% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_page_malloc_zero` | 1.0% | 3.6% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_page_xthread_id` | 1.0% | 1.0% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `<core::alloc::layout::Layout>::from_size_alignment_unchecked` | 0.9% | 0.9% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, quarkdown |
| `x64_sys_call` | 0.9% | 7.1% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `<krusty::name_tree::NameTree>::get` | 0.8% | 2.0% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
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
