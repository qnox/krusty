# krusty Analysis API session provider

The seam where krusty plugs into JetBrains' Kotlin Analysis API. A tool that analyses Kotlin
through the Analysis API (`analyze(ktElement) { … }`) asks the project's `KaSessionProvider` for a
`KaSession`. The standalone session registers the K2 compiler's provider, `KaFirSessionProvider`;
`KaKrustySessionProvider.install(project)` replaces it with krusty's.

Nothing is served yet: the session krusty's provider hands out refuses every member, naming it
(`krusty does not implement KaSymbolProvider.getNamedClassSymbol of the Analysis API yet`), so a
tool never receives an answer krusty has not computed. Components are served one at a time as
krusty gains the facts behind them.

## What the test proves

`SessionProviderTest` builds a standalone Analysis API session over one source module and the JDK,
the way KSP2 does. It asks the K2 compiler's provider for a class symbol, which is the oracle's
answer, installs krusty's provider in the same project, and checks that the same query is refused.

## Built against KSP2's Analysis API

KSP2 (`symbol-processing-aa-embeddable`) ships the Analysis API of the Kotlin release it targets
with every package moved under `ksp.` (`ksp.org.jetbrains.kotlin.analysis.api…`,
`ksp.com.intellij…`), so the adapter is written against those names. `-PkspVersion` selects the KSP
release (2.3.14 by default); the Analysis API is not a stable interface, so an adapter is built
for one KSP release at a time. JetBrains publishes the unrelocated artifacts
(`analysis-api-standalone-for-ide`) only with IDE-aligned versions, which do not name a Kotlin
release.

KSP2 does not call `buildStandaloneAnalysisAPISession`: `KotlinSymbolProcessing` builds its own
session and registers the services itself. The test registers the same application services
(permission registry and options, resolution activity tracker) that KSP registers.

```sh
./gradlew test    # tools/krusty-analysis-api
```
