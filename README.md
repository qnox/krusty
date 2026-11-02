# krusty conformance corpus

Each project is built from a pinned commit with its own Gradle build, once with kotlinc and
once with the krusty Gradle plugin, and the class files of every Kotlin/JVM compilation are
compared byte for byte. Written by the `oss corpus` workflow (`.github/workflows/oss-corpus.yml`
on master); `projects/` holds each project's latest result and `history.tsv` every run.

| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes | Measured |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| Exposed | 9,303 | 138,105 | 0 | ok | 0 / 43 | 0 / 8,100 (0.0%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| koin | 10,027 | 33,677 | 123 | ok | 5 / 19 | 96 / 3,705 (2.6%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| kotlinx.coroutines | 13,823 | 102,096 | 897 | ok | 0 / 34 | 0 / 9,482 (0.0%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| ktor | 14,562 | 259,026 | 85 | ok | 0 / 220 | 0 / 15,418 (0.0%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| leakcanary | 30,003 | 100,627 | 172 | ok | 6 / 34 | 23 / 1,798 (1.3%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| Maestro | 16,288 | 65,644 | 0 | ok | 1 / 19 | 27 / 2,156 (1.3%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| mirai | 14,796 | 156,078 | 559 | ok | 3 / 56 | 8 / 11,399 (0.1%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| moshi | 10,169 | 16,749 | 13,212 | ok | 5 / 12 | 45 / 512 (8.8%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| nowinandroid | 21,995 | 26,596 | 0 | ok | 17 / 58 | 156 / 1,110 (14.1%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| okhttp | 47,107 | 126,665 | 3,816 | ok | 9 / 38 | 40 / 1,483 (2.7%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| okio | 9,055 | 46,812 | 2,398 | failed (exit 1) | 1 / 6 | 12 / 343 (3.5%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| quarkdown | 16,348 | 87,771 | 23 | ok | 5 / 37 | 28 / 2,345 (1.2%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| Unciv | 11,518 | 139,231 | 1,184 | ok | 0 / 4 | 0 / 3,341 (0.0%) | [2026-11-02, krusty dd148ca](https://github.com/qnox/krusty/actions/runs/41409751998) |
| **Measured total** | | | | | **51 / 574** | **423 / 60,849 (0.7%)** | |

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
| Exposed | 0.0 s / 0.0 s | 306.0 s / 8.5 s | 3,671 MB / 195 MB | 5,136 MB / 2,107 MB | 166.4 s / 60.3 s | 577.9 s / 170.2 s |
| koin | 26.6 s / 8.3 s (3.2×) | 106.9 s / 35.0 s | 1,447 MB / 440 MB | 2,553 MB / 2,198 MB | 62.1 s / 63.6 s | 216.8 s / 182.8 s |
| kotlinx.coroutines | 0.0 s / 0.0 s | 193.6 s / 191.2 s | 3,239 MB / 27 MB | 5,044 MB / 5,123 MB | 160.2 s / 166.0 s | 531.5 s / 549.9 s |
| ktor | 0.0 s / 0.0 s | 634.5 s / 595.4 s | 4,348 MB / 47 MB | 9,480 MB / 9,570 MB | 377.4 s / 349.2 s | 1,261.4 s / 1,142.4 s |
| leakcanary | 22.5 s / 4.4 s (5.1×) | 129.8 s / 11.1 s | 2,110 MB / 308 MB | 4,018 MB / 2,589 MB | 74.2 s / 34.9 s | 260.3 s / 109.6 s |
| Maestro | 13.7 s / 1.0 s (13.7×) | 140.8 s / 7.7 s | 1,838 MB / 168 MB | 3,526 MB / 1,508 MB | 121.1 s / 57.9 s | 372.6 s / 156.4 s |
| mirai | 24.1 s / 5.5 s (4.4×) | 312.2 s / 32.3 s | 4,666 MB / 476 MB | 6,396 MB / 2,813 MB | 201.5 s / 76.3 s | 687.5 s / 245.4 s |
| moshi | 17.4 s / 4.9 s (3.6×) | 35.5 s / 7.5 s | 1,345 MB / 168 MB | 2,818 MB / 1,669 MB | 49.5 s / 33.0 s | 162.9 s / 91.3 s |
| nowinandroid | 85.3 s / 41.8 s (2.0×) | 215.9 s / 136.8 s | 3,779 MB / 300 MB | 7,317 MB / 6,522 MB | 143.7 s / 140.2 s | 529.6 s / 484.9 s |
| okhttp | 20.6 s / 14.9 s (1.4×) | 134.1 s / 54.9 s | 3,298 MB / 391 MB | 5,660 MB / 5,135 MB | 137.3 s / 108.9 s | 456.6 s / 350.3 s |
| okio | 2.2 s / 0.9 s (2.4×) | 34.4 s / 30.9 s | 2,088 MB / 138 MB | 3,807 MB / 3,845 MB | 104.1 s / 109.8 s | 337.4 s / 351.1 s |
| quarkdown | 23.8 s / 4.7 s (5.1×) | 180.5 s / 17.5 s | 1,690 MB / 276 MB | 3,107 MB / 2,241 MB | 111.2 s / 69.0 s | 403.7 s / 215.5 s |
| Unciv | 0.0 s / 0.0 s | 131.2 s / 141.3 s | 3,332 MB / 11 MB | 4,187 MB / 5,548 MB | 133.2 s / 154.0 s | 427.4 s / 499.2 s |
| **Total** | **236.2 s / 86.4 s (2.7×)** | **2,555.4 s / 1,270.1 s** | | | **1,841.9 s / 1,423.1 s** | **6,225.6 s / 4,549.0 s** |

## krusty hotspots

Sampled call stacks of the krusty processes of every krusty build (12,493 samples at 99 Hz).
*Self* counts the samples in the function itself, *total* those with it anywhere on the stack.

Flame graphs: [Exposed](projects/Exposed/krusty-flamegraph.svg), [koin](projects/koin/krusty-flamegraph.svg), [kotlinx.coroutines](projects/kotlinx.coroutines/krusty-flamegraph.svg), [ktor](projects/ktor/krusty-flamegraph.svg), [leakcanary](projects/leakcanary/krusty-flamegraph.svg), [Maestro](projects/Maestro/krusty-flamegraph.svg), [mirai](projects/mirai/krusty-flamegraph.svg), [moshi](projects/moshi/krusty-flamegraph.svg), [nowinandroid](projects/nowinandroid/krusty-flamegraph.svg), [okhttp](projects/okhttp/krusty-flamegraph.svg), [okio](projects/okio/krusty-flamegraph.svg), [quarkdown](projects/quarkdown/krusty-flamegraph.svg), [Unciv](projects/Unciv/krusty-flamegraph.svg).

| Function | Self | Total | Projects |
| --- | ---: | ---: | --- |
| `[libc.so.6]` | 5.1% | 98.7% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_theap_malloc_zero_aligned_at` | 3.4% | 13.0% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_free` | 3.0% | 12.0% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_block_nextx` | 2.6% | 2.6% | Exposed, Maestro, koin, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_free_block_local` | 2.1% | 3.9% | Exposed, Maestro, koin, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `zlib_rs::inflate::inflate_fast_help_avx2` | 2.0% | 2.5% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `<krusty::name_tree::Table>::probe` | 1.9% | 1.9% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `do_syscall_64` | 1.8% | 8.6% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_unchecked_ptr_page` | 1.6% | 3.5% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_theap_get_free_small_page` | 1.5% | 1.9% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `__rustc::__rust_dealloc` | 1.1% | 13.2% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_page_malloc_zero` | 1.0% | 4.1% | Exposed, Maestro, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_page_map_at` | 1.0% | 1.0% | Exposed, Maestro, koin, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, quarkdown |
| `mi_block_set_nextx` | 1.0% | 1.6% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `_mi_page_map_index` | 1.0% | 1.0% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `<krusty::name_tree::NameTree>::child_or_insert` | 0.9% | 2.4% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `<core::alloc::layout::Layout>::from_size_alignment_unchecked` | 0.8% | 0.8% | Exposed, Maestro, koin, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, quarkdown |
| `kernel_init_pages` | 0.8% | 0.8% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, quarkdown |
| `<krusty::jvm::classpath::Classpath>::jar_pkg_members_name` | 0.8% | 2.3% | Exposed, Maestro, koin, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |
| `mi_block_set_next` | 0.7% | 2.3% | Exposed, Maestro, Unciv, koin, kotlinx.coroutines, ktor, leakcanary, mirai, moshi, nowinandroid, okhttp, okio, quarkdown |

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
