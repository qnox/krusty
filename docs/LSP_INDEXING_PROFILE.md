# LSP indexing profile: IntelliJ Community

Profile date: 2026-08-01

## Conclusion

The indexing problem is primarily single-threaded algorithmic work, not insufficient
parallelism. The largest costs are global signature/type inference and repeated source-to-module
classification. Parsing and construction of the final navigation indexes are comparatively small.
The follow-up capture-discovery change confirms this directly: removing unrelated semantic checks
from one serial inference pass reduced the `acee6cd0` 1,000-file worker median by 38%, without
adding workers. The measured stacked prototype for dependency-preserving narrowing inside selected
classes removed another 15.7% from the resulting worker time. The return-inference dependency
worklist was then measured directly against its `d5a7c332` stack base at 7.40 versus 6.79 seconds,
an additional 8.3% reduction without adding workers. These are anchored branch measurements rather
than current-tree timings; absolute times vary with host load, and the integrated capture-discovery
implementation has not been re-profiled.

On the original profiling base (`2fc95b4b`), the two measured compiler-side changes reduce the
1,000-file analysis pass from 6.70 seconds to 3.86 seconds (42%) and peak RSS from 465 MiB to about
238 MiB (49%). On that same base, a project-model source-root trie reduces 10,000 source-to-module
lookups from 5.17 seconds to 0.01 seconds. These exact values are historical evidence from the
profiling branch, not fresh measurements of current `master`. The later `acee6cd0` validation below
confirms global return pre-inference remained dominant at that point and should be addressed before
adding analysis workers.

## Workload and host

- Repository: JetBrains `intellij-community`, commit
  `3cbdad9ee6c8a5135fc0f01cc90114fc25c0655c`.
- Checkout: a local worktree at the commit above; its machine-specific absolute path was not retained.
- Kotlin inventory: 80,475 `.kt` files (164.1 MiB) and 1,449 `.kts` files.
- Java inventory: 91,542 `.java` files.
- Static `.idea` inventory: 2,143 module references, 4,020 `sourceFolder` entries, and
  30,783 module dependency entries. The parsed JPS model expands this into 2,486 main/test
  compilation units, 1,654 usable source roots, and 61,332 dependency edges.
- Host: 4 physical CPUs, 7.8 GiB RAM, no swap.
- Krusty profiling base: `2fc95b4b`; build: optimized `release`; file lists were sorted and
  truncated identically between runs.

Unless explicitly described as rebased validation, timing, RSS, allocation, and sampled-CPU values
below were captured on `2fc95b4b` and its incremental profiling changes. The branch was later rebased
across 64 upstream commits onto `acee6cd0`; newer-master performance must be measured again before
using these values as a present-day baseline.

The analysis microbenchmarks use the first 1,000 or 2,000 Kotlin files below `platform`. They bypass
project discovery and exercise the same framed analysis-worker request used by the LSP. `open=0`
still performs global support-file parsing, signature collection, and return pre-inference, but does
not produce per-open-document results. This isolates indexing from diagnostics and response
serialization.

## Profiling-base scaling

| Workload | Worker time | Peak RSS | Source bytes |
|---|---:|---:|---:|
| Classpath only, cold | 0.15 s internal (3.79 s process) | 21 MiB | 0 |
| Classpath only, warm | 0.13 s internal (0.99 s process) | 21 MiB | 0 |
| 250 files, `open=1` | 1.26 s | 94 MiB | 0.83 MiB |
| 1,000 files, `open=1` | 6.94 s | 465 MiB | 5.85 MiB |
| 1,000 files, `open=0` | 6.70 s | 465 MiB | 5.85 MiB |
| 2,000 files, `open=1` | 22.47 s | 1,270 MiB | 10.03 MiB |

The 1,000-file `open=0` result is almost identical to `open=1`, so diagnostics/output for the open
file are not the problem. Doubling the file count from 1,000 to 2,000 increases time by 3.35x and
memory by 2.73x despite source bytes increasing only 1.71x. That is superlinear global analysis.

Classpath preparation is allocation-heavy but is not the large-source bottleneck. Heaptrack reports
about 1.53 million allocations, a 15.5 MiB peak heap, and 12.2 MiB live heap for classpath-only
startup. Adding 250 sources increases this by about 232,000 allocations and 4.2 MiB live heap, while
RSS rises by roughly 67 MiB. The difference indicates significant transient AST/map allocation plus
allocator retention or fragmentation; live heap alone understates process memory.

## CPU profile

The `2fc95b4b` baseline 1,000-file sample attributes inclusive CPU as follows:

| Operation | Baseline samples |
|---|---:|
| Whole source-set analysis | 89.5% |
| Signature collection | 55.3% |
| Name resolution against imports | 43.8% |
| Global return pre-inference | 30.6% |
| Call checking | 21.2% |
| Classpath package facades | 11.5% |
| Type-name resolution | 7.5% |
| Parsing | 3.3% |
| Definition-symbol index | 2.5% |
| Workspace-symbol index | 1.7% |

After the two compiler-side changes on that profiling base, the profile is:

| Operation | Post-change samples |
|---|---:|
| Whole source-set analysis | 85.3% |
| Global return pre-inference | 48.7% |
| Signature collection | 31.6% |
| Name resolution against imports | 16.0% |
| Type-name resolution | 10.9% |
| Call checking | 33.7% |
| Parsing | 4.7% |
| Definition-symbol index | 3.1% |
| Workspace-symbol index | 2.4% |

Percentages are inclusive and therefore overlap. On the profiling base, the post-change profile made
the next target clear: `preinfer_module_returns` repeatedly checks function bodies across the whole
inferred module until a fixpoint and accounted for nearly half of sampled CPU. Current `master` has
since changed inference behavior, so the next iteration must confirm this hotspot before modifying it.

### Later-base confirmation

The principal analysis profile was repeated on `acee6cd0`, including the final classifier and
source-root changes:

| Workload | Worker time | Peak RSS |
|---|---:|---:|
| 1,000 files, `open=0` | 12.52 s | 260 MiB |
| 2,000 files, `open=1` | 50.03 s | 415 MiB |

The sampled 1,000-file profile attributes 82.3% inclusive CPU to
`preinfer_module_returns_impl`, including 67.6% in
`discover_anonymous_object_captures_at`. Signature collection is 9.7%, classifier import resolution
5.0%, workspace-symbol indexing 1.7%, and definition-symbol indexing 1.3%. This confirms that global
pre-inference remained the next single-thread target on that validation base; it had become more
dominant, not less. Sampling itself changes wall time and RSS, so the table uses the matching
non-sampled run.

### Focused capture-discovery iteration

The rebased profile showed why anonymous-object capture discovery was unexpectedly expensive: if a
file contained any anonymous object, the scratch capture pass semantically checked every top-level
declaration in that file before return inference. Top-level declarations already use isolated
checker scopes, so declarations that do not lexically enclose an anonymous-object construction
cannot contribute a capture.

The follow-up pass structurally identifies the enclosing top-level functions, classes, and
properties and checks only those declarations. It still performs the complete ordered lexical walk
inside each selected declaration, preserving locals and types established before the construction.
On three interleaved, non-sampled A/B runs of the same optimized binaries and sorted 1,000-file
`platform` slice:

| Build | Median worker time | Median peak RSS |
|---|---:|---:|
| Rebased change (`26176907`) | 11.89 s | 266,228 KiB |
| Focused capture discovery | 7.42 s | 258,956 KiB |

That is a 37.6% worker-time reduction and a 2.7% peak-RSS reduction. Matching sampled runs reduced
inclusive capture-discovery samples from 6,769/9,379 (72.2%) to 2,836/5,877 (48.3%); the remaining
cost is the necessary check of the enclosing declarations, especially large classes. This is a
single-thread algorithmic improvement, not a parallel throughput result.

### Dependency-preserving class capture discovery

The next profile showed that selecting an enclosing class was still too coarse. Capture discovery
checked every instance, enum-entry, and companion method in that class. Only methods at or before an
anonymous-object construction can contribute lexical state, but the boundary is not simply the
construction-bearing method: instance and enum-entry methods publish inferred returns into shared
checker state which later methods and class regions can consume. On the sampled focused-capture binary,
`Checker::check_method` beneath capture discovery accounted for 2,231 of 5,877 samples (38.0% of the
entire process).

The follow-up therefore retains the complete ordered instance/enum method prefix through the last
construction-bearing method. If a construction occurs elsewhere in the class, it retains that whole
method sequence so a constructor, property, init block, enum argument, or companion function sees
the same inferred returns. Companion functions can be filtered individually because their checker
path does not publish inferred returns into a cross-function cache. Declaration validation and class
scope construction remain unchanged. Three interleaved, non-sampled A/B runs of the focused
capture-discovery and original dependency-preserving prototype binaries give:

| Build | Median worker time | Median peak RSS |
|---|---:|---:|
| Top-level capture scoping (`353b6e5f`) | 7.64 s | 258,924 KiB |
| Original dependency-preserving prototype | 6.44 s | 254,300 KiB |

That prototype showed a further 15.7% worker-time reduction and 1.8% peak-RSS reduction. In the
matching sampled run, capture discovery fell from 2,836/5,877 samples (48.3%) to 2,200/5,332
(41.3%), and its `check_method` subtree fell from 2,231 to 1,613 samples. The largest remaining
direct serial pass was `preinfer_returns_pass` at 1,664/5,332 samples (31.2%). Review integration
later replaced the prototype's resolver-local ownership rescans with the generic AST declaration
and function-root traversal. That exact integrated implementation has not been re-profiled, so the
measurements above establish the optimization direction but are not a current-tree timing claim.

### Return-inference dependency worklist

Tracing the fixpoint on the same 1,000-file slice found 125 files with inferred expression bodies.
Every round checked 35 top-level and 288 method candidates, but the first round changed only 43
signatures (31 methods, 10 top-level functions, and two member extensions), while the second changed
none. The unconditional second semantic sweep was therefore verification work rather than useful
inference for this workload.

The follow-up keeps the complete first round, then carries the names of changed callables into a
conservative file worklist. Only files with inferred expression bodies whose AST mentions a changed
callable are revisited. Import aliases are expanded. Names from the checker's canonical synthetic
operator registry and the remaining hidden syntax protocols retain every candidate file because
those calls can be implicit in the AST; ordinary infix functions keep their names and use the normal
AST scan. Java synthetic properties use the same platform getter-name mapping as resolution, cached
once per distinct AST name in each file. Name collisions can schedule extra work but cannot omit an
explicit or synthetic-property dependency.

Five interleaved optimized A/B runs give:

| Build | Median worker time | Median peak RSS |
|---|---:|---:|
| Dependency-preserving class capture discovery (`d5a7c332`) | 7.40 s | 254,060 KiB |
| Return-inference dependency worklist | 6.79 s | 253,596 KiB |

That is a further 8.3% worker-time reduction with effectively flat peak RSS. Median end-to-end time
falls from 7.89 to 7.26 seconds (8.0%). In the matching sampled profile,
`preinfer_returns_pass` falls from 1,664/5,332 samples (31.2%) to 1,162/5,373 (21.6%). The
worklist reduces this repeated single-thread work; it does not introduce analysis workers.

## Measured changes on the profiling base

### Share the compilation-wide class-name map

An equivalent `ClassNames::into_shared` implementation landed on `master` while this profiling
branch was in progress. It is included here to attribute the measured improvement, but is not
duplicated by this change set on the later validation base.

`ClassNames` already had an immutable `Rc<HashMap>` base plus a small per-file overlay, but the
compilation-wide project classes and consensus imports remained in the overlay. Cloning a per-file
resolver consequently copied the whole global map for every file, and another clone occurred per
class. Moving the completed global map into the shared base preserves independent overlays without
copying global state.

On 1,000 files this alone changes 6.70 seconds / 465 MiB to 5.55 seconds / 248 MiB. On 2,000 files it
changes 22.47 seconds / 1,270 MiB to 20.51 seconds / 399 MiB. This was the primary memory hog.

### Resolve classifiers without constructing callable scopes

Signature collection resolves type/classifier names. It previously called the general symbol-scope
resolver, eagerly loading package functions, extensions, and properties that cannot affect a
classifier result. The classifier path now probes candidate type names directly while preserving
import-level precedence, ambiguity, and type-alias behavior.

Together with shared class names, the 1,000-file result is 3.86 seconds / about 238 MiB. The 2,000-file
result is 17.18 seconds / 389 MiB, versus 22.47 seconds / 1,270 MiB at baseline.

### Index source roots by path component

`ProjectSources::load` classified each discovered path as many as four times. Each classification
linearly scanned every module and source root to find the longest prefix. On the real JPS model,
10,000 lookups took 5.17 seconds and accounted for 90% of that CPU profile. A component trie built
with `SourceModuleGraph` preserves longest-prefix, duplicate-root, and exact owning-root semantics and
reduces the same lookup set to 0.01 seconds. The later implementation also returns the owning
`SourceRoot`, so Java package-prefix handling does not fall back to a per-file linear scan. Project
model parsing itself took only 0.21-0.54 seconds on the profiling base.

The `acee6cd0` rerun parses the 2,486-unit JPS model in 0.16 seconds and builds the graph/trie in
0.04 seconds. It maps 10,000 Kotlin paths to modules in less than 0.01 seconds and 10,000 Java paths
to their exact owning roots in 0.01 seconds. The owning-root lookup preserves `package_prefix` while
eliminating the earlier code's remaining per-Java-file linear root scan.

### Reproducible profiler

The `memprofile` tool now supports sampled CPU flamegraphs and direct JPS-model profiling:

```sh
cargo build --release -p krusty-lsp --bin memprofile --features cpu-profile

INTELLIJ_COMMUNITY=/path/to/intellij-community

target/release/memprofile \
  --module "$INTELLIJ_COMMUNITY/platform" \
  --max 1000 --open 0 --stage 1 \
  --flamegraph /tmp/platform-1000.svg

target/release/memprofile \
  --jps-root "$INTELLIJ_COMMUNITY" \
  --max 10000 --flamegraph /tmp/intellij-jps.svg
```

Flamegraphs intentionally contain exact compiled Rust symbol names and may retain build-machine
details supplied by debug information. Keep generated SVGs outside the repository and inspect or
redact them before sharing; only aggregate measurements belong in committed documentation.

## Prioritized next improvements

1. Refine the return-inference worklist from conservative file/name dependencies to exact callable
   keys and declaration-level dependencies. The first round still checks every inferred expression
   body, and name collisions schedule unrelated second-round bodies. For an editor request, eagerly
   infer open files and lazily infer support bodies reached by those files. Cache the resulting
   declaration/return snapshot by source fingerprint. After the file worklist,
   `preinfer_returns_pass` contains 21.6% of inclusive samples.
2. Separate declaration/type-position names from arbitrary expression names during signature
   collection. `collect_file_type_names` intentionally over-approximates `Expr::Name`, including
   locals and parameters, causing useless import and classpath probes. A lexical local-name filter
   or a true type-position collector should reduce the current 5.0% import resolver cost.
3. Parse support files declaration-first and retain bodies only for open files or functions reached
   by return inference. Full AST bodies are currently retained through signature collection and
   global pre-inference, driving peak memory before body arenas can be released.
4. Memoize classifier results by `(package/import-scope fingerprint, name)` and intern decoded JVM
   library types. Many files share default imports and packages; post-change type lookup and library
   type construction are still visible CPU costs.
5. Add a `ModuleId -> module index` map and use a hash set for classpath deduplication. The JPS model
   has 61,332 expanded dependency edges, while `ProjectModel::module` and classpath deduplication are
   linear scans. This matters when several open modules require separate compiler configurations.
6. Avoid copying every source through JSON and worker-owned `String`s after the semantic changes
   above. A compact binary protocol, shared immutable source storage, or incremental source deltas
   can reduce peak RSS, but it will not fix the dominant inference complexity.

## Parallelization evaluation

Parallelism helps throughput only after separating work into independent module slices; it does not
fix the single-worker costs above.

On the pre-optimization build, four representative 1,000-file module slices (`platform`, `plugins`,
`python`, and `java`) take
27.64 seconds when run as separate sequential processes. Four concurrent workers finish in 9.21
seconds (3.0x throughput) but require roughly 1.64 GiB combined peak RSS. Two workers in two waves
finish in 17.43 seconds (1.6x) at roughly 0.85-0.94 GiB combined peak RSS. These figures are process
experiments, not a claim that the current LSP can safely partition an arbitrary module graph.

Recommended policy:

- Keep one analysis worker as the correctness and latency baseline.
- First land lazy/worklist return inference and declaration-only support parsing.
- If parallel analysis is then added, schedule independent module components, keep a shared immutable
  classpath/source snapshot, and use a memory budget rather than CPU count alone. On this 7.8 GiB,
  no-swap host, one or two workers are a safer initial ceiling than four.
- Measure first-result latency and peak RSS, not only total throughput. Open-document work should take
  priority over background indexing, with cancellation when edits invalidate a queued snapshot.

Parallelization is therefore a later throughput optimization. The measured primary solution is to
remove repeated work on one thread.

## Validation

- `cargo fmt --all -- --check`: passed.
- Resolver unit tests: 145 passed on the `acee6cd0` validation base.
- LSP project-model and project-source tests: 129 passed on the `acee6cd0` validation base.
- CPU-profiler feature build/test: passed.
- Full `./run-tests.sh` on the `acee6cd0` validation base: all test binaries passed.
- Full `./run-tests.sh` after focused capture discovery: all test binaries passed.
- Full `./run-tests.sh` on the original dependency-preserving prototype: all test binaries passed.
- Full `./run-tests.sh` for the return-inference dependency worklist on its stacked validation base:
  all test binaries passed.
