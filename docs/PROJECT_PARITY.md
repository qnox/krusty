# Project parity

How much of `intellij-community` krusty's front end accepts today. Regenerate with:

```bash
scripts/ij-parity.py --depth none
```

Project revision: `3f14adc599ef`
Harness revision: `6ad995d7840e`
Configuration: production roots, dependency depth `none`, one worker

This is the initial historical baseline. It predates the per-module
`visible_declarations` evidence now written by the harness, so its “declaration the scan could not
see” count used the older global file-stem heuristic. Regenerate before using that count as a
compiler-gap boundary; the raw error total and crash/module counts remain direct observations.

## How to read this

Each module is analyzed the way the language server would: its own Kotlin sources checked,
its Java sources supplied as stubs, and its declared jar classpath plus the platform JDK.
The scanned worktree has no built module outputs, so a reference into another module of the
same project cannot resolve. The current harness counts such an error separately only when the
named file-stem declaration exists in the project and was absent from that module's recorded
checked/inferred/Java inputs. This remains a deliberately conservative heuristic, not a pass-rate
adjustment. The historical baseline below used the older global-only form described above.

## Headline

| measure | value |
| --- | --- |
| modules scanned | 931 |
| modules with zero errors | 4 (0.4%) |
| modules whose only errors name an unbuilt dependency | 19 |
| Kotlin files checked | 25875 |
| error diagnostics | 634841 |
| … naming a declaration the scan could not see | 264449 |
| … remaining, clustered below | 370392 |
| distinct error clusters | 1006 |
| modules that hit the input budget | 0 |
| checked files that could not be read | 0 |

## Module outcomes

A module that timed out or crashed reports zero files and zero errors, so it counts in the
denominator above without contributing to any cluster.

| status | modules |
| --- | ---: |
| `errors` | 913 |
| `crash (signal: 10 (SIGBUS))` | 13 |
| `ok` | 4 |
| `crash (exit status: 101)` | 1 |

## Top error clusters

| errors | modules | pattern | example |
| ---: | ---: | --- | --- |
| 199148 | 879 | unresolved reference '_'. | `ClassicTerminalColorsMigration.kt:89` |
| 95198 | 832 | unresolved function '_' | `ClassicTerminalColorsMigration.kt:70` |
| 14637 | 637 | krusty: cannot infer the type of property '_'; add an explicit type | `LocalTerminalTtyConnector.kt:200` |
| 6808 | 410 | krusty: cannot destructure this type (no operator '_') | `ClassicTerminalColorsMigration.kt:65` |
| 4388 | 510 | krusty: supertype '_' could not be resolved (provide it on the classpath) | `ProxyTtyConnector.kt:6` |
| 4294 | 318 | krusty: this class-literal form is not supported | `LocalTerminalTtyConnector.kt:187` |
| 3886 | 353 | '_' is not an array (cannot index) | `ClassicTerminalColorsMigration.kt:88` |
| 3156 | 350 | argument type mismatch: actual type is '_', but '_' was expected. | `TerminalProjectOptionsProvider.kt:90` |
| 2917 | 64 | v0: class bodies support member '_', '_'/'_', and '_' blocks | `GithubResponsePage.kt:15` |
| 2782 | 310 | krusty: unresolved super method '_' | `LocalTerminalTtyConnector.kt:196` |
| 2588 | 309 | return label '_' does not denote an enclosing lambda | `GHPRProjectMetricsCollector.kt:103` |
| 2422 | 283 | no value passed for parameter '_'. | `LocalBlockTerminalRunner.kt:26` |
| 1824 | 229 | krusty: callable references are not supported | `BlockTerminalSession.kt:89` |
| 1583 | 254 | return type mismatch: expected '_', actual '_'. | `TerminalProjectOptionsProvider.kt:261` |
| 1489 | 168 | '_' cannot be reassigned. | `TerminalCommandSpecCompletionContributorGen1.kt:207` |
| 1335 | 321 | none of the following candidates is applicable: | `TerminalProjectOptionsProvider.kt:225` |
| 1286 | 197 | only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type '_'. | `TerminalFontSizeProviderImpl.kt:39` |
| 1263 | 245 | operator cannot be applied to '_' and '_' | `TerminalDeletePreviousWordAction.kt:22` |
| 1243 | 220 | unresolved Java static '_' for given argument types | `TerminalOptionsProvider.kt:95` |
| 984 | 241 | '_' expression must be exhaustive. Add an '_' branch. | `TerminalProjectOptionsProvider.kt:239` |
| 946 | 212 | krusty: catch type is not a known exception class | `LocalTerminalTtyConnector.kt:135` |
| 726 | 124 | expected an expression | `NewifyMemberContributor.kt:117` |
| 704 | 138 | no parameter with name '_' found. | `TerminalAbsolutePathLinkFinder.kt:114` |
| 701 | 142 | expected '_' | `GHPRReviewCommentBodyViewModel.kt:219` |
| 685 | 128 | assignment type mismatch: actual type is '_', but '_' was expected. | `TerminalOutputChangesTracker.kt:180` |

## Slowest modules

| ms | module |
| ---: | --- |
| 232268 | intellij.platform.ide.impl:main |
| 162263 | intellij.platform.lang.impl:main |
| 25571 | intellij.python.community.impl:main |
| 22082 | intellij.java.impl:main |
| 19706 | intellij.platform.vcs.impl:main |
| 14984 | intellij.vcs.git.backend:main |
| 13650 | intellij.maven:main |
| 12251 | intellij.groovy.psi:main |
| 12082 | intellij.platform.workspace.jps:main |
| 11457 | intellij.platform.ide:main |
| 10002 | intellij.platform.core.impl:main |
| 8581 | intellij.mermaid:main |
| 8292 | intellij.platform.util:main |
| 8110 | intellij.python.psi.impl:main |
| 7746 | intellij.groovy:main |
| 7464 | intellij.kotlin.projectWizard.core:main |
| 7011 | intellij.java.debugger.impl:main |
| 6795 | intellij.java.impl.refactorings:main |
| 6610 | intellij.platform.diff.impl:main |
| 6548 | intellij.java.ui:main |
| 6206 | intellij.platform.externalSystem.impl:main |
| 6143 | intellij.kotlin.j2k:main |
| 5942 | intellij.java.analysis.impl:main |
| 5856 | intellij.vcs.github:main |
| 5833 | intellij.grid.impl:main |
