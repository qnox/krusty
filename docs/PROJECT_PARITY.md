# Project parity

How much of `intellij-community` krusty's front end accepts today. Regenerate with:

```bash
scripts/parity-scan.py '<project-root>' --depth none --jobs 3 --timeout 300
```

The same command measures a checkout of the Kotlin compiler. Read the JSONL it writes
for modules scanned, modules that opened completely, missing classpath entries, error
diagnostics, crash rows, and the navigation timings. Parse time and type-information
latency stay unmeasured unless the server path timed them; do not copy `elapsed_ms`
into either. Those counts belong in the pull request that measured them.

Project revision: `b2c2e6113bcb04dfa18a73dcffca3de2532f1b6f`  
Compiler binary SHA-256: `d64b3f1d5e26387a7684a9a05487a6c4a615b5021dfc624388b84f3ad1c4edfd`  
Harness repository revision: `7153a17edc3d18731c0fe3a5872630e281244c5b`

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
| modules scanned | 1059 |
| modules with zero errors | 37 (3.5%) |
| modules whose only errors name an unbuilt dependency | 2 |
| Kotlin files checked | 15049 |
| error diagnostics | 447523 |
| … naming a declaration the scan could not see | 137463 |
| … remaining, clustered below | 310060 |
| distinct error clusters | 541 |
| modules that hit the input budget | 0 |
| checked files that could not be read | 0 |
| modules whose Java stub overlay failed | 0 |
| modules that opened completely | 555 |
| classpath entries missing from disk | 1970 |
| parse time | not measured |
| type-information latency | not measured |
| go-to-definition and find-references | definition 3903 µs, references 139758 µs (index lookup, not analysis) |

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
| `errors` | 920 |
| `crash (exit status: 101)` | 102 |
| `ok` | 37 |

## Top error clusters

| errors | modules | pattern | example |
| ---: | ---: | --- | --- |
| 224209 | 904 | unresolved reference '_'. | `DefaultGithubGistContentsCollector.kt:4` |
| 20617 | 718 | '_' overrides nothing. | `GHCopyLinkActionGroup.kt:15` |
| 19953 | 665 | unresolved reference '_' on receiver of type '_'. | `GHOpenInBrowserActionGroup.kt:39` |
| 5724 | 559 | internal error: Pass-N class has no finalized constructor signatures | `GHOpenInBrowserFromAnnotationActionGroup.kt:19` |
| 5130 | 150 | Cannot access '_' which is a supertype of '_'. Check your module classpath for missing or conflicting dependencies. | `GithubCreateGistService.kt:40` |
| 4739 | 433 | overload resolution ambiguity for extension property '_' | `GHGQLQueryLoader.kt:22` |
| 4738 | 516 | cannot infer type for type parameter '_'. Specify it explicitly. | `DefaultGithubGistContentsCollector.kt:22` |
| 3096 | 222 | Cannot access class '_'. Check your module classpath for missing or conflicting dependencies. | `DefaultGithubGistContentsCollector.kt:76` |
| 2263 | 265 | krusty: cannot destructure this type (no operator '_') | `DefaultGithubGistContentsCollector.kt:33` |
| 2064 | 304 | argument type mismatch: actual type is '_', but '_' was expected. | `GithubApiHelperRequestExecutor.kt:126` |
| 1441 | 326 | return type mismatch: expected '_', actual '_'. | `GHGQLRequests.kt:48` |
| 1353 | 232 | '_' cannot be reassigned. | `GithubLoginPanel.kt:58` |
| 1352 | 264 | krusty: unresolved super method '_' | `GHPRReviewSubmitAction.kt:99` |
| 1045 | 274 | unresolved reference '_' for operator '_'. | `DefaultGithubGistContentsCollector.kt:121` |
| 1044 | 224 | unresolved label. | `GHRepositoryHostingService.kt:26` |
| 833 | 134 | '_' is prohibited here. | `GHPRReviewCommentBodyViewModel.kt:166` |
| 650 | 9 | expected a class member declaration | `JavaProjectSettingsEntityImpl.kt:129` |
| 569 | 48 | no value passed for parameter '_'. | `GHPRVirtualFileSystem.kt:35` |
| 565 | 177 | '_' expression must be exhaustive. Add an '_' branch. | `GHPullRequestReviewThread.kt:72` |
| 532 | 132 | annotation argument must be a compile-time constant. | `GithubApiRequestExecutor.kt:51` |
| 488 | 113 | none of the following candidates is applicable: | `GithubApiRequest.kt:352` |
| 387 | 164 | unresolved Java static '_' for given argument types | `GHGQLQueryLoader.kt:30` |
| 377 | 94 | cannot infer type for value parameter '_'. Specify it explicitly. | `GithubApiHelperRequestExecutor.kt:125` |
| 369 | 119 | candidate '_' is inapplicable because of a receiver type mismatch. | `GHPRListViewModel.kt:86` |
| 333 | 16 | no parameter with name '_' found. | `JavaToJKTreeBuilder.kt:1023` |

## Slowest modules

| ms | module |
| ---: | --- |
| 18065 | intellij.platform.lang.impl:main |
| 14819 | intellij.platform.ide.impl:main |
| 8738 | intellij.platform.searchEverywhere.frontend:main |
| 7736 | intellij.platform.consoleView.impl:main |
| 6803 | intellij.vcs.git.shared:main |
| 6325 | intellij.platform.debugger.impl:main |
| 6030 | intellij.vcs.github:main |
| 5736 | intellij.kotlin.j2k:main |
| 4692 | intellij.maven:main |
| 4668 | intellij.platform.polySymbols.testFramework:main |
| 4652 | intellij.grid.impl:main |
| 4387 | intellij.vcs.git.backend:main |
| 4316 | intellij.vcs.gitlab.backend:main |
| 4301 | intellij.platform.locking.impl:main |
| 4091 | intellij.platform.workspace.jps:main |
| 4050 | intellij.platform.ide:main |
| 4044 | intellij.platform.recentFiles.frontend:main |
| 3832 | intellij.platform.vcs.impl:main |
| 3659 | intellij.platform.buildScripts.devDistGenerator:main |
| 3593 | intellij.platform.buildScripts.productDsl:main |
| 3267 | intellij.platform.structureView.frontend:main |
| 3198 | intellij.driver.sdk:main |
| 3129 | intellij.tools.ide.starter:main |
| 2919 | intellij.java.impl:main |
| 2910 | intellij.kotlin.projectWizard.core:main |

## Bazel worker: what actually builds

Measured on project revision `3f14adc599ef2c39a75c0e54c40d910676836bc8`, not on the revision in the headline. This open did not re-run the worker probe.

The numbers above measure ANALYSIS (diagnostics per file). This section measures EMISSION through
the Bazel path — `krusty --persistent_worker` speaking jvm-inc-builder's argument surface, driven by
`scripts/bazel-worker-probe.py`. No bazel install is involved: the worker protocol is line-delimited
JSON on stdin/stdout, so the probe drives it directly. That isolates krusty's side of the
integration; it does NOT exercise the Starlark rule, and the caveats below say where the two differ.

Only modules whose full transitive closure is Kotlin-only are attempted — 91 of 2570 (the probe also
skips closures above `--max-files`, inert at the default 400 on this checkout). krusty compiles no
Java, so a closure containing Java sources cannot be built from source at all. Modules are built in
dependency order and each produced jar is fed to its dependents' `--cp`, which is what an upstream
`krusty_jvm_library` target supplies.

| measure | value |
|---|---:|
| modules with a Kotlin-only closure | 91 |
| attempted (every Kotlin dependency built) | 42 |
| built to a jar | 6 |
| `.kt` files compiled | 39 |
| requests served by ONE worker process | 42 |

| outcome of an attempt | modules |
|---|---:|
| other diagnostic | 13 |
| unresolved reference | 11 |
| `unsupported by krusty` | 11 |
| internal compiler panic | 1 |

| not attempted | modules |
|---|---:|
| blocked upstream (a Kotlin dependency failed) | 47 |
| refused by the rule (`--friends`) | 2 |

The 47 blocked modules are deliberately not compiled and not counted as failures. Compiling a module
whose dependency never produced a jar measures an incomplete classpath, not the module: an earlier
version of this probe did exactly that and reported roughly twice as many "unresolved reference"
blockers as really exist.

**`built` is an upper bound.** The probe sends the compile-shaping options but not `--resources`,
`--java-count`, or `--abi-out`, which the real rule derives from the target. The worker now copies
`--resources` into the jar, so a target such as `intellij.platform.buildData` is no longer refused
for declaring `resources`. This scan predates that and did not send the flag.

`-Xlambdas=class` is the largest single refusal (10 of the 11 `unsupported` rows; the eleventh is
`-Xexplicit-api=strict`) and the refusal is correct: krusty emits `invokedynamic` lambdas only, so it
declines rather than emitting a shape it does not model. Note that `build/compiler-options.bzl` only
*defaults* `x_lambdas` to `indy`; 40 `BUILD.bazel` files — almost all under `fleet/` — pass
`x_lambdas = "class"` explicitly, and those are the modules that hit this. The bazel and JPS
configurations agree; the gap is krusty's.

The one panic is a compiler bug rather than a worker bug: it surfaces on
`intellij.kotlin.base.projectModel` as `invalid emitted metadata type: semantic type '<error>'`, and
the worker survived it — `catch_unwind` failed that single request and served the rest. Narrowing it
to a fixture is not part of this branch, so treat the module name and message as the only checked
facts.

Reproduce (needs `JAVA_HOME` pointing at a real JDK, and `cargo build --release`, which the test
harness does not produce):

    scripts/bazel-worker-probe.py --json probe.json
