# krusty conformance corpus

Each project is built from a pinned commit with its own Gradle build, once with kotlinc and
once with the krusty Gradle plugin, and the class files of every Kotlin/JVM compilation are
compared byte for byte. Written by the `oss corpus` workflow (`.github/workflows/oss-corpus.yml`
on master); `projects/` holds each project's latest result and `history.tsv` every run.

| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes | Measured |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| Exposed | 9,302 | 138,105 | 0 | ok | 0 / 43 | 0 / 8,100 (0.0%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| koin | 10,024 | 33,677 | 123 | ok | 4 / 19 | 79 / 3,705 (2.1%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| kotlinx.coroutines | 13,822 | 102,096 | 897 | ok | 0 / 34 | 0 / 9,482 (0.0%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| ktor | 14,557 | 259,026 | 85 | ok | 0 / 220 | 0 / 15,418 (0.0%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| leakcanary | 30,004 | 100,627 | 172 | ok | 6 / 34 | 23 / 1,798 (1.3%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| Maestro | 16,241 | 65,644 | 0 | ok | 1 / 19 | 26 / 2,156 (1.2%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| mirai | 14,792 | 156,078 | 559 | ok | 3 / 56 | 7 / 11,399 (0.1%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| moshi | 10,166 | 16,749 | 13,212 | ok | 0 / 12 | 0 / 512 (0.0%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| nowinandroid | 21,975 | 26,596 | 0 | ok | 16 / 58 | 144 / 1,110 (13.0%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| okhttp | 47,100 | 126,665 | 3,816 | ok | 9 / 38 | 40 / 1,483 (2.7%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| okio | 9,054 | 46,812 | 2,398 | failed (exit 1) | 0 / 6 | 0 / 343 (0.0%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| quarkdown | 16,339 | 87,771 | 23 | ok | 5 / 37 | 28 / 2,345 (1.2%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| Unciv | 11,496 | 139,231 | 1,184 | ok | 0 / 4 | 0 / 3,341 (0.0%) | [2026-10-28, krusty bf115cc](https://github.com/qnox/krusty/actions/runs/40648260963) |
| **Measured total** | | | | | **44 / 574** | **347 / 60,849 (0.6%)** | |

## Missed compilations

- **Exposed**
  - 42 × not run: a task it depends on failed
  - 1 × exposed-core/src/main/kotlin/org/jetbrains/exposed/v1/core/Alias.kt:41:90: error: cannot access 'fun clone(): Any!': it is protected in 'java.lang.Object'.
- **koin**
  - 11 × not run: a task it depends on failed
  - 1 × projects/core/koin-core-coroutines/src/commonMain/kotlin/org/koin/core/KoinWaitExt.kt:17:7: error: @file:JvmMultifileClass is not supported for JVM compilation yet; its part classes and delegating facade require distinct physical owners
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
  - 44 × not run: a task it depends on failed
  - 2 × left to kotlinc: krusty does not implement everything it requests (compilerOptions.optIn contains duplicate marker 'kotlinx.serialization.ExperimentalSerializationApi')
  - 1 × ci-release-helper/src/CiHelper.kt:198:68: error: krusty: empty arrayOf() needs an explicit type (unsupported)
  - 1 × mirai-console/tools/compiler-common/src/diagnostics/MiraiConsoleErrorsRendering.kt:40:9: error: unresolved reference 'put'.
  - 1 × mirai-console/tools/gradle-plugin/src/main/kotlin/IGNORED_DEPENDENCIES_IN_SHADOW.kt:10:7: error: @file:JvmMultifileClass is not supported for JVM compilation yet; its part classes and delegating facade require distinct physical owners
  - 1 × left to kotlinc: krusty does not implement everything it requests (compilerOptions.optIn contains duplicate marker 'kotlin.contracts.ExperimentalContracts')
  - 1 × left to kotlinc: krusty does not implement everything it requests (compilerOptions.optIn contains duplicate marker 'net.mamoe.mirai.utils.MiraiInternalApi')
  - 1 × mirai-core-utils/src/commonMain/kotlin/AtomicInteger.kt:21:28: error: overload resolution ambiguity between candidates:
  - 1 × mirai-deps-test/test/CoreDependencyResolutionTest.kt:27:6: error: annotation argument is not a supported compile-time constant
- **moshi**
  - 11 × not run: a task it depends on failed
  - 1 × moshi/src/main/java/com/squareup/moshi/-MoshiKotlinTypesExtensions.kt:80:61: error: unresolved reference 'asArrayType'.
- **nowinandroid**
  - 21 × left to kotlinc: compiler plugin androidx.compose.compiler.plugins.kotlin, argument -Xallow-unstable-dependencies
  - 9 × left to kotlinc: compiler plugin androidx.compose.compiler.plugins.kotlin, argument -Xuse-inline-scopes-numbers, argument -Xallow-unstable-dependencies
  - 8 × not run: a task it depends on failed
  - 2 × left to kotlinc: argument -Xuse-inline-scopes-numbers
  - 1 × core/data/src/main/kotlin/com/google/samples/apps/nowinandroid/core/data/repository/DefaultSearchContentsRepository.kt:1:1: error: the compiled inline body of `flatMapLatest` cannot be inlined: Regeneration(Unsupported("a $$forInline method without a continuation"))
  - 1 × core/datastore/src/test/kotlin/com/google/samples/apps/nowinandroid/core/datastore/IntToStringIdsMigrationTest.kt:1:1: error: internal error: module signatures were not finalized and no source diagnostic explains it
- **okhttp**
  - 24 × not run: a task it depends on failed
  - 4 × left to kotlinc: compiler plugin burst-kotlin-plugin
  - 1 × okhttp-tls/src/main/kotlin/okhttp3/tls/internal/der/Adapters.kt:38:68: error: unresolved reference 'readBoolean'.
- **okio**
  - 2 × left to kotlinc: compiler plugin burst-kotlin-plugin
  - 1 × okio-fakefilesystem/src/commonMain/kotlin/okio/fakefilesystem/FileMetadataCommon.kt:16:7: error: @file:JvmMultifileClass is not supported for JVM compilation yet; its part classes and delegating facade require distinct physical owners
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

## Gradle plugins

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `org.jetbrains.kotlin.plugin.scripting` | 13 | 344 |
| `org.jetbrains.kotlin.jvm` | 13 | 176 |
| `org.jetbrains.dokka` | 8 | 226 |
| `org.jetbrains.kotlin.multiplatform` | 8 | 168 |
| `org.jetbrains.kotlin.plugin.serialization` | 8 | 104 |
| `com.vanniktech.maven.publish.base` | 7 | 212 |
| `com.android.internal.version-check` | 7 | 104 |
| `com.vanniktech.maven.publish` | 5 | 189 |
| `com.android.internal.library` | 5 | 65 |
| `com.google.devtools.ksp` | 5 | 33 |
| `io.gitlab.arturbosch.detekt` | 4 | 79 |
| `com.diffplug.spotless` | 4 | 72 |
| `org.jetbrains.kotlinx.binary-compatibility-validator` | 4 | 23 |
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
| `symbol-processing-api` | 4 | 9 |
| `kotlin-compose-compiler-plugin-embeddable` | 3 | 22 |
| `burst-kotlin-plugin` | 2 | 5 |
| `kotlin-atomicfu-compiler-plugin-embeddable` | 1 | 18 |
| `purity-compiler-plugin` | 1 | 6 |
| `kotlin-allopen-compiler-plugin-embeddable` | 1 | 1 |
| `kotlin-build-tools-api` | 1 | 1 |
| `kotlin-compiler-embeddable` | 1 | 1 |
| `kotlin-daemon-embeddable` | 1 | 1 |
| `kotlin-parcelize-compiler` | 1 | 1 |
| `kotlinx-coroutines-core-jvm` | 1 | 1 |

## Compiler plugin arguments

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `-P plugin:androidx.compose.compiler.plugins.kotlin` | 2 | 5 |
