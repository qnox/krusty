# Kotlin compiler parity

How much of the Kotlin compiler repository krusty's front end accepts today. Regenerate with:

```bash
scripts/parity-scan.py '<project-root>' --depth none --jobs 3 --timeout 300
```

Project revision: `3c7a5041f238102285764d370665f56f587f9dc4`  
Compiler binary SHA-256: `55d5c8ae11d1d137d09fbedbf96dc4ff87f5265b0085fdef646337f207279ac6`  
Harness repository revision: `b7d5ea6062038d44ce27e979b4b4fa4d953664de` (dirty worktree)

## How to read this

Each module is analyzed the way the language server would: its own Kotlin sources checked,
its Java sources supplied as stubs, and its declared jar classpath plus the platform JDK.
The scanned worktree has no built module outputs, so a reference into another module of the
same project cannot resolve. Those errors are counted separately (they name a type the
project itself declares by file stem, and that stem was absent from the module's recorded
checked/inferred/Java inputs). They are NOT part of the clusters below. This is a
conservative heuristic, not an adjusted module pass rate.

## Headline

| measure | value |
| --- | --- |
| modules scanned | 455 |
| modules with zero errors | 14 (3.1%) |
| modules whose only errors name an unbuilt dependency | 0 |
| Kotlin files checked | 7838 |
| error diagnostics | 260447 |
| … naming a declaration the scan could not see | 60413 |
| … remaining, clustered below | 200034 |
| distinct error clusters | 583 |
| modules that hit the input budget | 0 |
| checked files that could not be read | 0 |
| modules whose Java stub overlay failed | 0 |
| modules that opened completely | 18 |
| classpath entries missing from disk | 14993 |
| parse time | not measured |
| type-information latency | not measured |
| go-to-definition and find-references | definition 1128 µs, references 93201 µs (index lookup, not analysis) |

Parse time and type-information latency are not measured: one module analysis is timed
as a whole (`elapsed_ms`), and that figure is not copied into either of them. Go-to-definition
and find-references are the server's index lookups after that analysis, in microseconds,
and only for modules that issued the query. Find-references reads the open documents'
definition indexes. The closed-file index keeps diagnostics and parse-only symbols, not
those edges, so it is not searched.

## Module outcomes

A module that timed out or crashed reports zero files and zero errors, so it counts in the
denominator above without contributing to any cluster.

| status | modules |
| --- | ---: |
| `errors` | 381 |
| `crash (exit status: 101)` | 60 |
| `ok` | 14 |

## Top error clusters

| errors | modules | pattern | example |
| ---: | ---: | --- | --- |
| 150345 | 370 | unresolved reference '_'. | `FirAnnotationContainer.kt:23` |
| 20688 | 313 | unresolved reference '_' on receiver of type '_'. | `FirFunctionTypeParameterBuilder.kt:41` |
| 6655 | 217 | '_' overrides nothing. | `FirErrorTypeRef.kt:40` |
| 2303 | 202 | internal error: Pass-N class has no finalized constructor signatures | `FirContractElementDeclarationImpl.kt:19` |
| 2198 | 6 | '_' and '_' declarations can be used only in multiplatform projects. Learn more about Kotlin Multiplatform: https://kotl.in/multiplatform-setup | `_Arrays.kt:697` |
| 1768 | 158 | krusty: cannot destructure this type (no operator '_') | `FirThreadUnsafeCachesFactory.kt:82` |
| 1317 | 15 | This part of the Build Tools API is experimental and might change in the future releases | `CompilerArgumentCompatibilityTest.kt:87` |
| 988 | 158 | the feature "_" is only available since language version N.N | `FirThreadUnsafeCachesFactory.kt:82` |
| 911 | 125 | argument type mismatch: actual type is '_', but '_' was expected. | `FirCachesFactory.kt:171` |
| 810 | 104 | '_' cannot be reassigned. | `FirAnonymousFunctionBuilder.kt:120` |
| 599 | 87 | '_' is prohibited here. | `declarationAttributes.kt:187` |
| 555 | 78 | '_' is not an array (cannot index-assign) | `NullableMap.kt:34` |
| 484 | 37 | Cannot access class '_'. Check your module classpath for missing or conflicting dependencies. | `CapturedWrites.kt:22` |
| 482 | 82 | krusty: unresolved super method '_' | `CapturedWrites.kt:116` |
| 478 | 115 | cannot infer type for value parameter '_'. Specify it explicitly. | `FirThreadUnsafeCachesFactory.kt:56` |
| 462 | 47 | expected an expression | `KotlinCompilationCompilerOptionsConfigurator.kt:20` |
| 428 | 63 | operator '_' cannot be applied to '_' and '_' | `ConeIntegerLiteralTypeImpl.kt:86` |
| 424 | 72 | return type mismatch: expected '_', actual '_'. | `Utils.kt:313` |
| 408 | 88 | '_' expression must be exhaustive. Add an '_' branch. | `FirSession.kt:73` |
| 388 | 58 | unresolved reference '_' for operator '_'. | `FirDefaultPropertyBackingField.kt:41` |
| 385 | 9 | Cannot access '_' which is a supertype of '_'. Check your module classpath for missing or conflicting dependencies. | `localVariableReferences.kt:17` |
| 364 | 72 | unresolved label. | `FirDeclarationRendererWithAttributes.kt:23` |
| 323 | 84 | none of the following candidates is applicable: | `Utils.kt:346` |
| 315 | 22 | no context argument for '_' found. | `FirConflictsHelpers.kt:350` |
| 310 | 10 | no applicable standard collection factory '_' | `FirRawContractDescriptionBuilder.kt:23` |

## Slowest modules

| ms | module |
| ---: | --- |
| 9212 | :analysis:analysis-api-fir-diagnostics:main |
| 8728 | :compiler:fir:checkers:checkers.common:main |
| 5334 | :compiler:fir:tree:main |
| 4626 | :kotlin-stdlib:jvm/main |
| 3164 | :analysis:analysis-api-fir:main |
| 2894 | :tools:kotlin-stdlib-gen:main |
| 2602 | :analysis:decompiled:decompiler:main |
| 1993 | :compiler:ir.tree:testFixtures |
| 1540 | :compiler:build-tools:kotlin-build-tools-compat:main |
| 1506 | :native:kotlin-klib-commonizer:main |
| 1451 | :kotlin-build-common:main |
| 1315 | :compiler:build-tools:kotlin-build-tools-impl:main |
| 1280 | :kotlin-reflect:main |
| 1203 | :compiler:build-tools:kotlin-build-tools-api:main |
| 1141 | :compiler:fir:raw-fir:light-tree2fir:main |
| 1111 | :compiler:frontend:main |
| 1087 | :native:swift:sir-providers:main |
| 1071 | :plugins:compose-compiler-plugin:compiler-hosted:main |
| 1054 | :analysis:symbol-light-classes:main |
| 1048 | :compiler:jvm.tests:testFixtures |
| 975 | :core:compiler.common:main |
| 942 | :plugins:parcelize:parcelize-compiler:parcelize.backend:main |
| 940 | :kotlin-lombok-compiler-plugin.k2:main |
| 925 | :compiler:ir.tree:main |
| 894 | :compiler:multiplatform-parsing:jvm/main |
