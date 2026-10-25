# krusty conformance corpus

Each project is built from a pinned commit with its own Gradle build, once with kotlinc and
once with the krusty Gradle plugin, and the class files of every Kotlin/JVM compilation are
compared byte for byte. Written by the `oss corpus` workflow (`.github/workflows/oss-corpus.yml`
on master); `projects/` holds each project's latest result and `history.tsv` every run.

| Project | Stars | Kotlin LOC | Java LOC | Reference build | Compilations built by krusty | Identical classes | Measured |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| Exposed | 9,301 | 138,105 | 0 | ok | 0 / 43 | 0 / 8,100 (0.0%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| koin | 10,022 | 33,677 | 123 | ok | 4 / 19 | 76 / 3,705 (2.1%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| kotlinx.coroutines | 13,824 | 102,096 | 897 | failed (exit 1) | 0 / 0 | 0 / 0 | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| ktor | 14,554 | 259,031 | 85 | failed (exit 1) | 0 / 0 | 0 / 0 | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| leakcanary | 30,003 | 100,627 | 172 | ok | 6 / 34 | 22 / 1,798 (1.2%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| Maestro | 16,207 | 65,644 | 0 | ok | 1 / 19 | 26 / 2,156 (1.2%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| mirai | 14,792 | 156,072 | 559 | ok | 0 / 51 | 0 / 2,536 (0.0%) | [2026-10-25, krusty cf34959](https://github.com/qnox/krusty/actions/runs/40199492852) |
| moshi | 10,166 | 16,749 | 13,212 | ok | 0 / 12 | 0 / 512 (0.0%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| nowinandroid | 21,966 | 26,596 | 0 | ok | 24 / 58 | 221 / 1,110 (19.9%) | [2026-10-25, krusty cf34959](https://github.com/qnox/krusty/actions/runs/40199492852) |
| okhttp | 47,095 | 126,665 | 3,816 | failed (exit 1) | 2 / 2 | 7 / 14 (50.0%) | [2026-10-25, krusty cf34959](https://github.com/qnox/krusty/actions/runs/40199492852) |
| okio | 9,052 | 46,812 | 2,398 | failed (exit 1) | 0 / 6 | 0 / 343 (0.0%) | [2026-10-25, krusty cf34959](https://github.com/qnox/krusty/actions/runs/40199492852) |
| quarkdown | 16,323 | 87,771 | 23 | ok | 0 / 37 | 0 / 2,345 (0.0%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| Unciv | 11,486 | 139,231 | 1,184 | ok | 0 / 4 | 0 / 3,341 (0.0%) | [2026-10-25, krusty 0e127f3](https://github.com/qnox/krusty/actions/runs/40196119897) |
| **Measured total** | | | | | **35 / 277** | **345 / 25,603 (1.3%)** | |

## Missed compilations

- **Exposed**
  - 43 × krusty wrote no class
- **koin**
  - 11 × krusty wrote no class
  - 1 × projects/core/koin-core-coroutines/src/commonMain/kotlin/org/koin/core/KoinWaitExt.kt:17:7: error: @file:JvmMultifileClass is not supported for JVM compilation yet; its part classes and delegating facade require distinct physical owners
  - 1 × projects/core/koin-core/src/commonTest/kotlin/org/koin/core/ClosedScopeAPI.kt:253:42: error: type mismatch: inferred type is T but Scope was expected
  - 1 × projects/core/koin-test/src/commonMain/kotlin/org/koin/test/check/CheckModules.kt:154:22: error: none of the following candidates is applicable:
  - 1 × projects/ktor/koin-ktor/src/commonMain/kotlin/org/koin/ktor/ext/ApplicationExt.kt:22:29: error: unresolved reference 'KOIN_ATTRIBUTE_KEY'.
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
  - 51 × krusty wrote no class
- **moshi**
  - 11 × krusty wrote no class
  - 1 × moshi/src/main/java/com/squareup/moshi/-JsonValueReader.kt:348:31: error: return type mismatch: expected 'Void?', actual 'Nothing?'.
- **nowinandroid**
  - 21 × left to kotlinc: krusty does not implement everything it requests (krusty does not implement the kotlinc argument '-Xallow-unstable-dependencies'; krusty: unsupported compiler plugin '/home/runner/.gradle/caches/modules-2/files-2.1/org.jetbrains.kotlin/kotlin-compose-compiler-plugin-embeddable/2.4.21/dd3cf05b1c6bfaf6c82869e1a0e5cf00df6b26b1/kotlin-compose-compiler-plugin-embeddable-2.4.21.jar': krusty has no native implementation for it and cannot host FIR/IR compiler plugins (only codegen processors via KSP). Ignoring it would silently produce wrong output, so this is an error — remove the plugin or compile this module with kotlinc.; krusty: unsupported compiler plugin 'plugin id 'androidx.compose.compiler.plugins.kotlin'': krusty has no native implementation for it and cannot host FIR/IR compiler plugins (only codegen processors via KSP). Ignoring it would silently produce wrong output, so this is an error — remove the plugin or compile this module with kotlinc.)
  - 9 × left to kotlinc: krusty does not implement everything it requests (krusty does not implement the kotlinc argument '-Xuse-inline-scopes-numbers'; krusty does not implement the kotlinc argument '-Xallow-unstable-dependencies'; krusty: unsupported compiler plugin '/home/runner/.gradle/caches/modules-2/files-2.1/org.jetbrains.kotlin/kotlin-compose-compiler-plugin-embeddable/2.4.21/dd3cf05b1c6bfaf6c82869e1a0e5cf00df6b26b1/kotlin-compose-compiler-plugin-embeddable-2.4.21.jar': krusty has no native implementation for it and cannot host FIR/IR compiler plugins (only codegen processors via KSP). Ignoring it would silently produce wrong output, so this is an error — remove the plugin or compile this module with kotlinc.; krusty: unsupported compiler plugin 'plugin id 'androidx.compose.compiler.plugins.kotlin'': krusty has no native implementation for it and cannot host FIR/IR compiler plugins (only codegen processors via KSP). Ignoring it would silently produce wrong output, so this is an error — remove the plugin or compile this module with kotlinc.)
  - 1 × core/data/src/test/kotlin/com/google/samples/apps/nowinandroid/core/data/repository/OfflineFirstNewsRepositoryTest.kt:323:18: error: overload resolution ambiguity
  - 1 × left to kotlinc: krusty does not implement everything it requests (krusty does not implement the kotlinc argument '-Xuse-inline-scopes-numbers')
  - 1 × core/datastore/src/test/kotlin/com/google/samples/apps/nowinandroid/core/datastore/IntToStringIdsMigrationTest.kt:1:1: error: internal error: module signatures were not finalized and no source diagnostic explains it
  - 1 × left to kotlinc: krusty does not implement everything it requests (krusty does not implement the kotlinc argument '-Xuse-inline-scopes-numbers'; krusty does not implement the kotlinc argument '-Xallow-unstable-dependencies')
- **okio**
  - 2 × left to kotlinc: krusty does not implement everything it requests (krusty: unsupported compiler plugin '/home/runner/.gradle/caches/modules-2/files-2.1/app.cash.burst/burst-kotlin-plugin/2.14.0/18ea5fbb5fc62dbafd0552da69c94e5d1059e59e/burst-kotlin-plugin-2.14.0.jar': krusty has no native implementation for it and cannot host FIR/IR compiler plugins (only codegen processors via KSP). Ignoring it would silently produce wrong output, so this is an error — remove the plugin or compile this module with kotlinc.)
  - 1 × okio-fakefilesystem/src/commonMain/kotlin/okio/fakefilesystem/FileMetadataCommon.kt:16:7: error: @file:JvmMultifileClass is not supported for JVM compilation yet; its part classes and delegating facade require distinct physical owners
  - 1 × okio/jvm/jmh/src/jmh/java/com/squareup/okio/benchmarks/BenchmarkUtils.kt:31:12: error: unresolved reference 'commonToUtf8String'.
  - 1 × krusty: error: -Xjdk-release=8 compiles against the JDK 8 API from ct.sym, which krusty does not implement; the selected JDK is 21
  - 1 × krusty wrote no class
- **quarkdown**
  - 37 × krusty wrote no class
- **Unciv**
  - 4 × krusty wrote no class

## Gradle plugins

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `org.jetbrains.kotlin.gradle.scripting.internal.ScriptingKotlinGradleSubplugin` | 13 | 197 |
| `org.jetbrains.kotlin.plugin.scripting` | 13 | 197 |
| `org.jetbrains.kotlin.jvm` | 13 | 163 |
| `org.gradle.build-init` | 12 | 257 |
| `org.gradle.help-tasks` | 12 | 257 |
| `org.gradle.software-reporting-tasks` | 12 | 257 |
| `org.gradle.wrapper` | 12 | 257 |
| `org.gradle.lifecycle-base` | 12 | 237 |
| `org.gradle.kotlin.kotlin-dsl-project` | 12 | 231 |
| `org.gradle.base` | 12 | 230 |
| `org.gradle.reporting-base` | 12 | 228 |
| `org.gradle.jvm-ecosystem` | 12 | 224 |
| `org.gradle.java-base` | 12 | 220 |
| `org.gradle.jvm-toolchains` | 12 | 220 |
| `org.gradle.java` | 12 | 122 |
| `org.gradle.jvm-test-suite` | 12 | 122 |
| `org.gradle.test-suite-base` | 12 | 122 |
| `org.jetbrains.kotlin.gradle.plugin.BuildMetricsPlugin` | 10 | 174 |
| `org.jetbrains.kotlin.plugin.serialization` | 8 | 70 |
| `org.gradle.maven-publish` | 7 | 115 |
| `org.gradle.publishing` | 7 | 115 |
| `org.gradle.signing` | 7 | 113 |
| `com.android.internal.version-check` | 7 | 107 |
| `com.android.build.gradle.api.AndroidBasePlugin` | 7 | 77 |
| `org.jetbrains.kotlin.gradle.plugin.KotlinBaseApiPlugin` | 7 | 61 |
| `org.jetbrains.dokka.gradle.DokkaBasePlugin` | 6 | 115 |
| `org.jetbrains.dokka.gradle.adapters.JavaAdapter` | 6 | 113 |
| `org.jetbrains.dokka.gradle.adapters.KotlinAdapter` | 6 | 113 |
| `org.jetbrains.dokka` | 6 | 90 |
| `com.vanniktech.maven.publish.base` | 6 | 89 |

## Kotlin compiler plugins

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `annotations` | 13 | 197 |
| `kotlin-stdlib` | 13 | 197 |
| `kotlin-script-runtime` | 13 | 196 |
| `kotlin-scripting-common` | 13 | 195 |
| `kotlin-scripting-compiler-embeddable` | 13 | 195 |
| `kotlin-scripting-compiler-impl-embeddable` | 13 | 195 |
| `kotlin-scripting-jvm` | 13 | 195 |
| `kotlin-serialization-compiler-plugin-embeddable` | 7 | 30 |
| `symbol-processing-api` | 4 | 9 |
| `kotlin-compose-compiler-plugin-embeddable` | 3 | 22 |
| `kotlin-reflect` | 3 | 3 |
| `burst-kotlin-plugin` | 2 | 5 |
| `gradle-api` | 2 | 2 |
| `gradle-installation-beacon` | 2 | 2 |
| `gradle-kotlin-dsl` | 2 | 2 |
| `gradle-kotlin-dsl-shared-runtime` | 2 | 2 |
| `gradle-kotlin-dsl-tooling-models` | 2 | 2 |
| `groovy` | 2 | 2 |
| `groovy-ant` | 2 | 2 |
| `groovy-astbuilder` | 2 | 2 |
| `groovy-datetime` | 2 | 2 |
| `groovy-dateutil` | 2 | 2 |
| `groovy-groovydoc` | 2 | 2 |
| `groovy-json` | 2 | 2 |
| `groovy-nio` | 2 | 2 |
| `groovy-templates` | 2 | 2 |
| `groovy-xml` | 2 | 2 |
| `javaparser-core` | 2 | 2 |
| `kotlin-assignment-compiler-plugin-embeddable` | 2 | 2 |
| `kotlin-sam-with-receiver-compiler-plugin-embeddable` | 2 | 2 |

## Compiler plugin arguments

| Plugin | Projects | Modules |
| --- | ---: | ---: |
| `-P` | 2 | 5 |
