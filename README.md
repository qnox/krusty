# krusty conformance corpus

Each project is built from a pinned commit with its own Gradle build, once with kotlinc and
once with the krusty Gradle plugin, and the class files of every Kotlin/JVM compilation are
compared byte for byte. Written by the `oss corpus` workflow (`.github/workflows/oss-corpus.yml`
on master); `projects/` holds each project's latest result and `history.tsv` every run.

| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes | Measured |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| Exposed | 9,301 | 138,105 | 0 | ok | 0 / 43 | 0 / 8,100 (0.0%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| koin | 10,022 | 33,677 | 123 | ok | 4 / 19 | 76 / 3,705 (2.1%) | [2026-10-25, krusty e911a36](https://github.com/qnox/krusty/actions/runs/40212685036) |
| kotlinx.coroutines | 13,824 | 102,096 | 897 | ok | 0 / 34 | 0 / 9,482 (0.0%) | [2026-10-25, krusty b5979a1](https://github.com/qnox/krusty/actions/runs/40201721357) |
| ktor | 14,554 | 259,026 | 85 | ok | 0 / 220 | 0 / 15,418 (0.0%) | [2026-10-25, krusty e911a36](https://github.com/qnox/krusty/actions/runs/40212685036) |
| leakcanary | 30,004 | 100,627 | 172 | ok | 6 / 34 | 22 / 1,798 (1.2%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| Maestro | 16,208 | 65,644 | 0 | ok | 1 / 19 | 26 / 2,156 (1.2%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| mirai | 14,793 | 156,072 | 559 | ok | 0 / 51 | 0 / 2,536 (0.0%) | [2026-10-25, krusty b5979a1](https://github.com/qnox/krusty/actions/runs/40201721357) |
| moshi | 10,166 | 16,749 | 13,212 | ok | 0 / 12 | 0 / 512 (0.0%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| nowinandroid | 21,967 | 26,596 | 0 | ok | 24 / 58 | 221 / 1,110 (19.9%) | [2026-10-25, krusty cf34959](https://github.com/qnox/krusty/actions/runs/40199492852) |
| okhttp | 47,095 | 126,665 | 3,816 | ok | 9 / 38 | 37 / 1,483 (2.5%) | [2026-10-25, krusty 323386f](https://github.com/qnox/krusty/actions/runs/40208384415) |
| okio | 9,052 | 46,812 | 2,398 | failed (exit 1) | 0 / 6 | 0 / 343 (0.0%) | [2026-10-25, krusty e911a36](https://github.com/qnox/krusty/actions/runs/40212685036) |
| quarkdown | 16,323 | 87,771 | 23 | ok | 0 / 37 | 0 / 2,345 (0.0%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| Unciv | 11,486 | 139,231 | 1,184 | ok | 0 / 4 | 0 / 3,341 (0.0%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| **Measured total** | | | | | **44 / 569** | **382 / 51,986 (0.7%)** | |

## Missed compilations

- **Exposed**
  - 43 × krusty wrote no class
- **koin**
  - 11 × not run: a task it depends on failed
  - 1 × projects/core/koin-core-coroutines/src/commonMain/kotlin/org/koin/core/KoinWaitExt.kt:17:7: error: @file:JvmMultifileClass is not supported for JVM compilation yet; its part classes and delegating facade require distinct physical owners
  - 1 × projects/core/koin-core/src/commonTest/kotlin/org/koin/core/ClosedScopeAPI.kt:253:42: error: type mismatch: inferred type is T but Scope was expected
  - 1 × projects/core/koin-test/src/commonMain/kotlin/org/koin/test/check/CheckModules.kt:154:22: error: none of the following candidates is applicable:
  - 1 × projects/ktor/koin-ktor/src/commonMain/kotlin/org/koin/ktor/ext/ApplicationExt.kt:22:29: error: unresolved reference 'KOIN_ATTRIBUTE_KEY'.
- **kotlinx.coroutines**
  - 30 × left to kotlinc: krusty does not implement everything it requests (freeCompilerArg '-progressive' conflicts with compilerOptions.progressiveMode; configure the structured Gradle input instead)
  - 4 × left to kotlinc: krusty does not implement everything it requests (freeCompilerArg '-progressive' conflicts with compilerOptions.progressiveMode; configure the structured Gradle input instead; freeCompilerArg '-progressive' conflicts with compilerOptions.progressiveMode; configure the structured Gradle input instead)
- **ktor**
  - 220 × left to kotlinc: argument -Xreport-all-warnings, argument -Xrender-internal-diagnostic-names
- **leakcanary**
  - 24 × krusty wrote no class
  - 2 × left to kotlinc: krusty does not implement everything it requests (freeCompilerArg '-P' conflicts with compiler plugin configuration; configure the structured Gradle input instead)
  - 1 × leakcanary/leakcanary-deobfuscation-gradle-plugin/src/main/java/com/squareup/leakcanary/deobfuscation/LeakCanaryLeakDeobfuscationPlugin.kt:50:57: error: argument type mismatch: actual type is 'Any', but 'Variant' was expected.
  - 1 × shark/shark-hprof/src/main/java/shark/HprofDeobfuscator.kt:1:1: error: internal error: checked FIR construction failed: Check(BodyCheckFailure { span: Some(Span { lo: 4167, hi: 4254 }), kind: UnpublishableType(Error) })
- **Maestro**
  - 14 × krusty wrote no class
  - 1 × maestro-ai/src/main/java/maestro/ai/cloud/ApiClient.kt:1:1: error: krusty: this construct is not yet supported by the IR backend
  - 1 × maestro-ios-driver/src/main/kotlin/util/LocalSimulatorUtils.kt:239:50: error: unresolved reference 'Path'.
  - 1 × maestro-utils/src/test/kotlin/MaestroTimerTest.kt:19:9: error: unresolved reference 'assertEquals'.
  - 1 × maestro-web/src/main/kotlin/maestro/web/cdp/CdpClient.kt:1:1: error: krusty: JVM backend inline error: call arity mismatch
- **mirai**
  - 51 × not run: the krusty build failed: Found class org.jetbrains.kotlin.gradle.dsl.KotlinTopLevelExtension, but interface was expected
- **moshi**
  - 11 × krusty wrote no class
  - 1 × moshi/src/main/java/com/squareup/moshi/-JsonValueReader.kt:348:31: error: return type mismatch: expected 'Void?', actual 'Nothing?'.
- **nowinandroid**
  - 21 × left to kotlinc: compiler plugin androidx.compose.compiler.plugins.kotlin, argument -Xallow-unstable-dependencies
  - 9 × left to kotlinc: compiler plugin androidx.compose.compiler.plugins.kotlin, argument -Xuse-inline-scopes-numbers, argument -Xallow-unstable-dependencies
  - 1 × core/data/src/test/kotlin/com/google/samples/apps/nowinandroid/core/data/repository/OfflineFirstNewsRepositoryTest.kt:323:18: error: overload resolution ambiguity
  - 1 × left to kotlinc: argument -Xuse-inline-scopes-numbers
  - 1 × core/datastore/src/test/kotlin/com/google/samples/apps/nowinandroid/core/datastore/IntToStringIdsMigrationTest.kt:1:1: error: internal error: module signatures were not finalized and no source diagnostic explains it
  - 1 × left to kotlinc: argument -Xuse-inline-scopes-numbers, argument -Xallow-unstable-dependencies
- **okhttp**
  - 24 × not run: a task it depends on failed
  - 2 × left to kotlinc: compiler plugin burst-kotlin-plugin
  - 2 × left to kotlinc: krusty does not implement everything it requests (freeCompilerArg '-module-name=okhttp' conflicts with compilerOptions.moduleName; configure the structured Gradle input instead)
  - 1 × okhttp-tls/src/main/kotlin/okhttp3/tls/internal/der/Adapters.kt:38:68: error: unresolved reference 'readBoolean'.
- **okio**
  - 2 × left to kotlinc: compiler plugin burst-kotlin-plugin
  - 1 × okio-fakefilesystem/src/commonMain/kotlin/okio/fakefilesystem/FileMetadataCommon.kt:16:7: error: @file:JvmMultifileClass is not supported for JVM compilation yet; its part classes and delegating facade require distinct physical owners
  - 1 × okio/jvm/jmh/src/jmh/java/com/squareup/okio/benchmarks/BenchmarkUtils.kt:31:12: error: unresolved reference 'commonToUtf8String'.
  - 1 × krusty: error: -Xjdk-release=8 compiles against the JDK 8 API from ct.sym, which krusty does not implement; the selected JDK is 21
  - 1 × not run: a task it depends on failed
- **quarkdown**
  - 37 × krusty wrote no class
- **Unciv**
  - 4 × krusty wrote no class

## Gradle plugins

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `org.jetbrains.kotlin.plugin.scripting` | 13 | 345 |
| `org.jetbrains.kotlin.jvm` | 13 | 177 |
| `org.jetbrains.dokka` | 8 | 226 |
| `org.jetbrains.kotlin.multiplatform` | 8 | 168 |
| `com.android.internal.version-check` | 8 | 108 |
| `org.jetbrains.kotlin.plugin.serialization` | 8 | 104 |
| `com.vanniktech.maven.publish.base` | 7 | 212 |
| `com.android.internal.library` | 6 | 69 |
| `com.vanniktech.maven.publish` | 5 | 189 |
| `com.google.devtools.ksp` | 5 | 33 |
| `io.gitlab.arturbosch.detekt` | 4 | 79 |
| `com.diffplug.spotless` | 4 | 72 |
| `org.jetbrains.kotlinx.binary-compatibility-validator` | 4 | 23 |
| `com.android.internal.application` | 4 | 7 |
| `com.google.osdetector` | 4 | 4 |
| `org.jetbrains.kotlinx.kover` | 3 | 43 |
| `org.jetbrains.kotlin.plugin.compose` | 3 | 22 |
| `org.jetbrains.kotlinx.atomicfu` | 2 | 148 |
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
| `kotlin-serialization-compiler-plugin-embeddable` | 7 | 62 |
| `symbol-processing-api` | 4 | 9 |
| `kotlin-compose-compiler-plugin-embeddable` | 3 | 22 |
| `burst-kotlin-plugin` | 2 | 5 |
| `kotlin-stdlib-common` | 1 | 49 |
| `kotlin-gradle-plugin-api` | 1 | 40 |
| `kotlin-native-utils` | 1 | 40 |
| `kotlin-project-model` | 1 | 40 |
| `kotlin-serialization` | 1 | 40 |
| `kotlin-tooling-core` | 1 | 40 |
| `kotlin-util-io` | 1 | 40 |
| `kotlin-atomicfu-compiler-plugin-embeddable` | 1 | 18 |
| `kotlin-jvm-blocking-bridge-compiler-embeddable` | 1 | 6 |
| `purity-compiler-plugin` | 1 | 6 |
| `kotlin-dynamic-delegation-compiler-embeddable` | 1 | 3 |
| `kotlin-allopen-compiler-plugin-embeddable` | 1 | 1 |
| `kotlin-build-tools-api` | 1 | 1 |
| `kotlin-compiler-embeddable` | 1 | 1 |
| `kotlin-daemon-embeddable` | 1 | 1 |
| `kotlin-parcelize-compiler` | 1 | 1 |
| `kotlinx-coroutines-core-jvm` | 1 | 1 |

## Compiler plugin arguments

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `-P plugin:androidx.compose.compiler.plugins.kotlin` | 1 | 4 |
| `-P` | 1 | 1 |
