# Project parity

How much of `intellij-community` krusty's front end accepts today. Regenerate with:

```bash
scripts/parity-scan.py '<project-root>' --depth none --jobs 6 --timeout 300
```

Project revision: `3f14adc599ef2c39a75c0e54c40d910676836bc8` (dirty worktree)  
Compiler binary SHA-256: `d7a39acd3f91042421d2e4c363e44088362885ce978f9048ac91a5561b25c8ab`  
Harness repository revision: `0ce1ae4ef3ec426536fe80fac11922dfa36772fe`

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
| modules scanned | 931 |
| modules with zero errors | 4 (0.4%) |
| modules whose only errors name an unbuilt dependency | 19 |
| Kotlin files checked | 25134 |
| error diagnostics | 627220 |
| … naming a declaration the scan could not see | 252981 |
| … remaining, clustered below | 374239 |
| distinct error clusters | 977 |
| modules that hit the input budget | 0 |
| checked files that could not be read | 0 |
| modules whose Java stub overlay failed | 0 |

## Module outcomes

A module that timed out or crashed reports zero files and zero errors, so it counts in the
denominator above without contributing to any cluster.

| status | modules |
| --- | ---: |
| `errors` | 916 |
| `crash (signal: 10 (SIGBUS))` | 10 |
| `ok` | 4 |
| `timeout` | 1 |

## Top error clusters

| errors | modules | pattern | example |
| ---: | ---: | --- | --- |
| 301247 | 891 | unresolved reference '_'. | `DefaultGithubGistContentsCollector.kt:32` |
| 13590 | 640 | krusty: cannot infer the type of property '_'; add an explicit type | `DefaultGithubGistContentsCollector.kt:144` |
| 6924 | 412 | krusty: cannot destructure this type (no operator '_') | `DefaultGithubGistContentsCollector.kt:33` |
| 4532 | 515 | krusty: supertype '_' could not be resolved (provide it on the classpath) | `GHRepositoryCoordinates.kt:9` |
| 4288 | 322 | krusty: this class-literal form is not supported | `GHGQLRequests.kt:239` |
| 3870 | 356 | '_' is not an array (cannot index) | `GithubApiRequest.kt:259` |
| 3015 | 66 | v0: class bodies support member '_', '_'/'_', and '_' blocks | `GithubResponsePage.kt:15` |
| 2970 | 354 | argument type mismatch: actual type is '_', but '_' was expected. | `GHEServerVersionChecker.kt:15` |
| 2642 | 307 | krusty: unresolved super method '_' | `GHPRTimelineVirtualFile.kt:47` |
| 2535 | 312 | return label '_' does not denote an enclosing lambda | `GHPRProjectMetricsCollector.kt:103` |
| 1761 | 232 | krusty: callable references are not supported | `GHOpenInBrowserFromAnnotationActionGroup.kt:33` |
| 1526 | 257 | return type mismatch: expected '_', actual '_'. | `DefaultGithubGistContentsCollector.kt:126` |
| 1296 | 323 | none of the following candidates is applicable: | `GithubApiRequest.kt:181` |
| 1229 | 170 | '_' cannot be reassigned. | `Utils.kt:23` |
| 1205 | 248 | operator cannot be applied to '_' and '_' | `GHOpenInBrowserFromAnnotationActionGroup.kt:28` |
| 1112 | 201 | only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type '_'. | `GHPullRequest.kt:82` |
| 1111 | 143 | only named arguments are available for Java annotations. | `GHShareProjectUtil.kt:15` |
| 1068 | 221 | unresolved Java static '_' for given argument types | `GHGQLQueryLoader.kt:30` |
| 988 | 244 | '_' expression must be exhaustive. Add an '_' branch. | `GHPRAISummaryViewModel.kt:50` |
| 925 | 216 | krusty: catch type is not a known exception class | `GithubLoginPanel.kt:97` |
| 706 | 16 | krusty: object bodies support '_', '_'/'_', and '_' blocks | `GithubIssuesLoadingHelper.kt:30` |
| 702 | 142 | no parameter with name '_' found. | `GithubApiRequestExecutor.kt:216` |
| 652 | 112 | expected an expression | `NewifyMemberContributor.kt:117` |
| 633 | 116 | expected '_' | `NewifyMemberContributor.kt:117` |
| 597 | 134 | krusty: a lambda that calls a local function is not supported | `GHGQLQueryLoader.kt:30` |

## Slowest modules

| ms | module |
| ---: | --- |
| 300046 | intellij.platform.ide.impl:main |
| 212532 | intellij.platform.lang.impl:main |
| 32504 | intellij.python.community.impl:main |
| 24641 | intellij.platform.vcs.impl:main |
| 22475 | intellij.vcs.git.backend:main |
| 22314 | intellij.java.impl:main |
| 20134 | intellij.maven:main |
| 16948 | intellij.platform.ide:main |
| 15947 | intellij.groovy.psi:main |
| 12447 | intellij.python.psi.impl:main |
| 10598 | intellij.groovy:main |
| 8734 | intellij.devkit.core:main |
| 7680 | intellij.vcs.github:main |
| 7209 | intellij.platform.execution.impl:main |
| 6547 | intellij.gradle:main |
| 6489 | intellij.platform.debugger.impl:main |
| 6460 | intellij.kotlin.j2k:main |
| 6359 | intellij.mermaid:main |
| 6312 | intellij.java.analysis.impl:main |
| 6121 | intellij.platform.util:main |
| 5619 | intellij.platform.diff.impl:main |
| 5550 | intellij.platform.workspace.jps:main |
| 5182 | intellij.platform.vcs.log.impl:main |
| 5151 | intellij.grid.impl:main |
| 4038 | intellij.platform.externalSystem.impl:main |
