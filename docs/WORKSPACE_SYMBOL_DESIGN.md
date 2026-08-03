# `workspace/symbol` — project-wide symbol search

## Status

`workspace/symbol` is implemented. `WorkspaceSymbolIndex` in `crates/krusty-lsp/src/analysis.rs` is
built per analysis batch from the analyzed source set; the capability and dispatch arm landed in
"add bounded workspace symbols" (#383).

Landed since:

- No entry ceiling. The index once capped itself at 32 * 1024 entries, so declarations past it were
  unfindable regardless of the query; only the response is bounded now (#403).
- Ranked matching over two orderings of entry indices — by lowercased name and by camel-hump
  initials — with rungs for name prefix, initials prefix, initials subsequence, and name
  subsequence, plus an `is_complete` flag reporting whether every declaration fit the snapshot
  budget (#403).
- Declaration names interned into the index rather than sliced out of retained source text (#403).
- Query forms beyond a bare substring: wildcards, package-qualified queries, wrong-keyboard-layout
  mapping, and the trailing-`::` client-filter escape.
- **Project-wide, in-memory coverage.** The index has its own URI table, so `entry[0]` names a file
  rather than addressing a position in the retained source set; `document_symbol_occurrences` takes
  the parsed `File`, so an unopened file is indexed by a parse alone; the engine raises a parse-only
  symbol pass over the project model's source inventory; that index persists across analysis
  batches; and the live analysis layer shadows it per file, so an edited buffer wins over its text
  on disk. This retired
  `workspace_symbols_do_not_observe_the_snapshot_after_the_last_document_closes`.

- **Discovery on shared walkers.** Every source root goes into one threaded walk rather than one
  walk per root, which is what makes threading it worth anything: a root on its own is a few dozen
  directories.

**Still outstanding:** everything from "Composite index" onward — persistence across restarts and
dependency indices — is design for remaining work, not a description of current behaviour. Coverage
is in place and discovery is no longer the cold-start cost; what is missing is surviving a restart
and extending the index past the workspace's own sources.

## Problem

Symbol search used to cover only what the live analysis snapshot held. `textDocument/documentSymbol`
reads `self.documents`, which is open files alone; `WorkspaceSymbolIndex` widened that to the
analyzed source set, which is the modules `ProjectSources` loaded for those open documents. Neither
reached a file nobody had opened, so a project-wide picker could not find most declarations in a
large repository. The project layer described below is what closed that gap; the rest of this
document is the design it came from and the work still ahead of it.

## Client behaviour we must design against

All of the following was read out of Zed's source, not assumed. It constrains the design more than
the LSP specification does.

1. **Capability gate.** `lsp_store.rs` skips a server outright unless
   `workspace_symbol_provider` is `Some`. Both flat `SymbolInformation[]` and nested
   `WorkspaceSymbol[]` responses are accepted.

2. **No `workspaceSymbol/resolve`.** A nested symbol whose `location` is the URI-only variant is
   dropped with `log::error!("Unexpected: client capabilities forbid symbol resolutions in
   workspace.symbol.resolveSupport")`. **Every returned symbol must carry a full location with a
   range.** The 3.17 resolve model is unavailable here.

3. **A request per keystroke.** `ProjectSymbolsDelegate::update_matches` issues a fresh
   `workspace/symbol` and replaces the entire result set each time the query changes.

4. **Client-side re-filtering.** Results are re-matched with `fuzzy::match_strings` — a subsequence
   matcher — and re-sorted client-side. The server's ranking is advisory at best.

5. **The filtered text is the `name` we return.** With no Kotlin adapter providing
   `labels_for_symbols`, Zed builds `CodeLabel::plain(name, None)`; `plain` delegates to
   `filtered(text, text.len(), None, vec![])`, whose `filter_range` defaults to the whole string.
   So `filter_text() == name`. The server controls the string the client matches against.

6. **An empty client query disables filtering.** `match_strings` starts with
   `if query.is_empty() { return candidates.iter().map(...).collect() }` — every candidate passes,
   each scored `0.`.

7. **`::` splits the client query.** `query.rsplit_once("::")` keeps only the suffix for client-side
   filtering, while the *full* query still goes to the server. This is an existing accommodation for
   rust-analyzer's path syntax.

8. **Caps.** `MAX_MATCHES = 100` client-side. Because an empty query scores everything `0.`, the
   sort key `(Reverse(score), filter_text)` collapses to alphabetical order.

Facts 6 + 7 combine into a verified escape hatch: **a query ending in `::` empties the client-side
filter**, making the server the sole authority over matching. Facts 2 and 8 are hard limits.

The LSP specification itself defines nothing here. `WorkspaceSymbolParams.query` is documented only
as "A query string to filter symbols by. Clients may send an empty string here to request all
symbols." No matching semantics, no wildcard syntax. All query behaviour is server-defined.

## Measurements

Taken on the two reference corpora with a parse-only walk (release build, 8 threads, 10-core
darwin/arm64, 32 GB). These numbers drive the design decisions below.

### Corpus scale and build cost

| | kotlin | intellij-community |
|---|---|---|
| files / source bytes | 64,648 · 129.3 MiB | 48,306 · 71.1 MiB |
| symbols extracted | 698,516 | 365,822 |
| unique names | 173,551 | 121,279 |
| retained index | 38.3 MiB | 24.3 MiB |
| — packed entries | 21.3 MiB | 11.2 MiB |
| — name table | 7.2 MiB | 5.0 MiB |
| — URI table | 9.8 MiB | 8.1 MiB |
| peak RSS during build | 125.7 MiB | 68.4 MiB |
| build wall (8 threads) | 5.1s | 3.2s |
| directory walk (cold / warm) | 7.9s / 1.8s | 11.0s / 5.0s |

Peak RSS runs roughly 3x the retained size because parse arenas churn during the build.

### Query strategies

Best of 25 rounds over the kotlin name table (173,551 unique names, 3.26 MiB folded blob):

| query | per-name substring | blob scan | sorted prefix | camel initials | hits (substring / prefix / initials) |
|---|---|---|---|---|---|
| `a` | 3.06 ms | 13.54 ms | <0.001 ms | <0.001 ms | 126225 / 15250 / 15294 |
| `re` | 6.68 ms | 7.75 ms | <0.001 ms | <0.001 ms | 38680 / 6389 / 358 |
| `res` | 5.31 ms | 4.38 ms | <0.001 ms | <0.001 ms | 7878 / 1101 / 15 |
| `file` | 4.24 ms | 3.73 ms | <0.001 ms | <0.001 ms | 3835 / 402 / 0 |
| `resolve` | 4.26 ms | 2.64 ms | <0.001 ms | <0.001 ms | 2354 / 671 / 0 |
| `psf` | 4.89 ms | 1.74 ms | <0.001 ms | <0.001 ms | 9 / 0 / 36 |
| `psifile` | 3.76 ms | 2.72 ms | <0.001 ms | <0.001 ms | 18 / 5 / 0 |

Binary search over a sorted id array is four orders of magnitude cheaper than any scan, and the
camel-initials array finds matches (`psf` → 36) that substring search structurally cannot (9).

**A trigram or FST index is not justified.** It would add 7+ MiB to beat a 4 ms worst case that only
fires on the fallback rung.

### Change rates (branch switching)

| | kotlin | intellij-community |
|---|---|---|
| tracked `.kt` | 64,778 | 48,722 |
| `HEAD~1` | 1 (0.00%) | 2 (0.00%) |
| `HEAD~10` | 23 (0.04%) | 12 (0.02%) |
| `HEAD~100` | 323 (0.50%) | 387 (0.79%) |
| `HEAD~1000` | 4,586 (7.08%) | 1,800 (3.69%) |
| `HEAD~5000` | 33,725 (52.06%) | 6,365 (13.06%) |

At ~80–100 µs to parse and extract one file, a realistic branch switch (10–100 commits) is
**10–40 ms of work**. Re-indexing a delta is not a performance problem.

### Discovery

| | kotlin |
|---|---|
| directory walk (cold / warm) | 7.9s / 1.8s |
| `git ls-files '*.kt'` | 0.77s |
| `git diff --name-only HEAD~100 HEAD` | 1.03s |
| `git status --porcelain` | 1.12s |

**Cold start dominates everything.** Walking the tree and parsing it is the cost; incremental
updates are noise. This inverts the usual argument for persistence: it is a startup optimisation,
not a branch-switching one.

The walk itself is now threaded, which is where that cost went. It waits on directory reads rather
than working the CPU, so more walkers than cores is the point — over a single large tree, 4 walkers
give 1.5x, 8 give 1.8x, 16 give 2.3x and 32 give 2.7x, and sixteen is where intellij-community stops
improving.

That measurement is not the one that matters, though, and taking it for the answer was a mistake
worth recording. **Neither caller walks one large tree.** Both walk the project model's source
roots, and a source root is usually a few dozen package directories — so a set of walkers per root
sets itself up and tears itself down for a handful of reads, which measured 2-3x *slower* than not
threading at all. Every root now goes into one walk, which is both the correct shape and the one
with enough work to divide:

| | roots | per-root, single-threaded | one walk, shared walkers |
|---|---|---|---|
| kotlin | 903 | 303 ms | 195 ms |
| intellij-community | 839 | 1,109 ms | 564 ms |

Measured interleaved in one process, because the machine drifts between runs by more than the change
is worth: separate runs of the same code varied by 80%.

The shared queue remains correct when the process cannot create a helper thread: worker creation is
fallible, and if every request is refused the caller thread drains the same queue instead. Each
claimed directory is also guarded until release, so an unexpected worker panic marks the traversal
failed and wakes waiters rather than leaving the in-flight count permanently nonzero. The caller
boundary owns the meaning of that failure: strict semantic discovery discards the partial prefix,
while workspace inventory retains it and marks the index incomplete.

`git ls-files` would read the same set faster still, but it buys a dependency on the git binary, a
fallback path for every workspace that is not a repository, and a second definition of what counts
as a workspace source — for a cost that is already well under a second.

## Index structure

`WorkspaceSymbolIndex` holds `entries: Vec<[u32; 13]>` (file, selection span, pre-resolved start/end
line and character, kind, parent, package id, name id, declaration span), interned `names` and
`packages` tables, two
permutations of entry indices (`by_name`, `by_initials`) that make the cheap rungs binary searches,
and a `complete` flag.

It also holds a `files` table: `entry[0]` addresses the index's own files rather than a position in
the retained source set, which is what lets an entry describe a file whose text nothing holds. Names
were interned first; the URI table was the other half of that coupling.

A freshly built index is still positional — the builder runs in the analysis worker, which is handed
sources and never sees a URI. `assign_uris` ends that phase at the batch the server applies, the one
place that knows which document each position was. `merge_from` unions file tables by URI, so two
indexes built from different source sets merge without either one's numbering leaking into the
other. Those phases are a runtime invariant rather than a debug-only convention: URI assignment is
one-shot, and a merge refuses either direction when one non-empty side is positional and the other
is bound. In both cases the valid entries are preserved and the index becomes incomplete. This is
necessary because `entry[0] == 0` is valid in both representations, so inspecting an individual
entry cannot recover which meaning was intended after an invalid merge.

The layout stays pointer-free and `u32`-addressed, matching the AST/IR convention and, not
coincidentally, what a memory-mapped format would require.

## Symbol extraction

`document_symbol_occurrences` (`crates/krusty-lsp/src/compiler_analysis/document_symbols.rs`) is
shared by `documentSymbol` and `workspace/symbol`, so kinds and ranges agree between them.

Indexing unopened files needs a parse without resolution, so `document_symbol_occurrences` takes the
parsed `File` rather than a `FileAnalysis`; nothing in it reads resolved types. `parsed_file_symbols`
feeds it from `krusty::frontend::parse_source_with_detected_features`, and lives in
`compiler_analysis` so the retained-index layer above never depends on the AST — the architecture
test enforces that boundary.

It is guarded by `MAX_INDEXED_FILE_BYTES`, checked against file metadata before the read so an
oversized file costs a `stat` rather than a parse. A multi-megabyte generated or sparse file
otherwise stalls the build for tens of seconds, which is how a 32 MiB test fixture once starved the
request loop. The disk adapter feeds that metadata-filtered iterator directly into the same generic
URI/source builder used by borrowed test inputs, so only one file's owned text is retained at a
time. Because a metadata rejection happens before that builder can observe the source, the adapter
also marks the resulting index incomplete explicitly; an oversized-only chunk must not look like a
complete empty chunk.

## Query language

Server-side grammar, with a compatibility rule for Zed's client-side filter.

### Works natively (no escape syntax)

| feature | mechanism |
|---|---|
| fuzzy / prefix | subsequence-compatible; the client filter agrees with ours |
| CamelHump (`PSF`) | `by_initials`; the expansion is a subsequence of the name |
| package-qualified (`kotlin.collections.listOf`) | **adaptive naming**: when the query contains `.` or `/`, return the *qualified* name in the `name` field, so the query is a literal subsequence of it and survives the client filter |

Adaptive naming is what makes IntelliJ-style package search work in Zed with no user-visible syntax.
Simple names are returned for unqualified queries so the picker stays readable.

### Requires the `::` escape

These contain characters that appear in no symbol name, so Zed's subsequence filter would drop every
result. Appending `::` empties the client filter (facts 6 + 7 above) and hands matching to the
server.

| feature | example |
|---|---|
| wildcards | `*Parse*::`, `Fo?Bar::` |
| wrong keyboard layout | `зфкыу::` → `parse` |

Trade-off in this mode: results sort alphabetically and cap at 100, because the client scores every
candidate `0.`. Acceptable for power queries; not the default path.

The same mechanism explains why rust-analyzer's own `#` and `*` markers are broken in Zed today. The
durable fix is upstream — Zed already special-cases `::`, so honouring a server-declared "do not
re-filter" is a small, precedented change. Worth filing regardless of what ships here.

### Cost of answering

Both fallback rungs are scans, so a project-wide index has to keep the per-entry work to nothing:
the lowercase name and camel-hump expansion are derived once per interned name and held beside the
name table, and a rung tests the name table (173,551) rather than the entries (698,516).

The response is capped by symbol count as well as by bytes. A broad query matches tens of thousands
of declarations, and encoding them was the dominant cost of answering — for a client that keeps 100.
Results are emitted strongest rung first, so the cap keeps the best of them, and because encoding
stops at the cap the weakest rungs are never scanned at all.

Measured on a synthetic corpus of the reference shape (698,880 declarations, 6 segments, release
build): 20–30 ms per query, against 123–526 ms before those two changes, and 81 ms for a query that
matched nothing. Re-indexing one edited file is 12.7 ms, against 132 ms when removal re-sorted both
rank orders instead of renumbering them.

### Cost against the index

- **Wildcards** — extract the literal prefix before the first metacharacter, binary-search that
  range, then glob-verify inside it. `Foo*` is sub-microsecond; `*Foo*` degrades to the 4 ms
  substring scan. `?` consumes one Unicode scalar rather than one UTF-8 byte, and a shared
  transition budget bounds adversarial wildcard backtracking. The budget is owned by the whole
  request and is spent across every project segment, live layer, ranking rung, and translated query;
  resetting it at any inner loop would multiply the bound by attacker-controlled index structure.
  No new index structure.
- **Keyboard layout** — a ЙЦУКЕН→QWERTY positional table applied to the *query*; search both forms
  and union. The explicit mapping touches Cyrillic characters only, so ordinary qualified-query
  punctuation does not spuriously create a translated query. ~200 bytes, zero index cost.

## Composite index

Four layers. For source layers, the highest layer holding a path wins; a deleted path gets a
tombstone.

```
edit overlay     in-memory, unsaved buffers              volatile
commit deltas    segments per commit since the baseline  small, mmap'd
baseline         full snapshot at commit C               mmap'd, ~38 MiB
dependencies     one index per artifact                  mmap'd, shared across workspaces
```

Two of these exist today, both in memory. The edit overlay is the live analysis index. The baseline
is `ProjectSymbolIndex`, and it is already segmented rather than singular: each chunk becomes a
segment, and adjacent segments merge only while their sizes are within a factor of two, so a
declaration is rewritten a logarithmic number of times instead of once per later chunk. Measured on
the corpus shape, folding every chunk into one index cost 54 ms at 20k entries and grew from there,
on the thread that also serves requests; segmenting holds the median splice near 5 ms with under a
dozen resident segments. The 192 MiB project budget belongs to the aggregate rather than an
individual merge: `replace_files` reserves the conservative retained cost of every untouched
segment before admitting a replacement, including a tail too differently sized to merge. This
keeps the promised whole-layer ceiling true between compactions as well as immediately after one.

Shadowing is per file, and layer order decides shadowing only — ranking walks rung by rung across
every layer, or a subsequence match in a newly indexed chunk would outrank an exact prefix match in
the segment beside it. `replace_files` is the tombstone: every URI a chunk *attempted* is
re-indexed, so a file that was deleted or became unreadable loses its entries rather than keeping
them forever.

The shadow set is the union of the open documents and the files the live layer names, and it needs
both halves. The live layer names only files some entry references, so a buffer edited down to no
declarations names nothing and would let the project layer answer with the file's text from disk;
the open set alone misses the support sources analysis pulls in beside the open ones, which would be
reported from two layers at once. `didClose` drops the closed file from the live layer, because a
layer that names a file is a layer that hides the one below it.

What is missing is making any of it survive a restart, and compaction is currently unbounded in the
sense that it only ever merges — there is no eviction.

This is the segment model Lucene uses (immutable segments, tombstones, background compaction) and
the milestone model IntelliJ's shared indexes use — a prebuilt index for a nearby commit, with local
indexing layered on top, keyed by content hash rather than local file ids so it is portable.

### Dependency layer

The highest-leverage part, because it is shared across workspaces.

- **Key each index by jar content hash, not by Maven/Gradle coordinates.** Coordinates lie —
  snapshots, republished artifacts, local builds. A content hash is exact and makes sharing safe.
- **One file per jar** in a shared cache (`~/.cache/krusty/deps/<hash>.symidx`), not per workspace.
  Ten projects depending on `kotlin-stdlib` map one file; the page cache holds a single copy. The
  entire Gradle cache measured here is 436,289 classes / 37.4 MiB of FQNs — indexed once, ever.
- **Never invalidated.** Content-addressed and immutable, so there is no staleness logic at all —
  only LRU eviction against a size cap. A large class of correctness risk simply does not exist here.
- **The JDK gets the same treatment** — one index per JDK image (28,251 classes measured on
  JBR 21).
- Class names come from `Classpath::package_tree()`, which already holds
  `classes: Vec<(NameId, JarId)>` (`src/jvm/classpath.rs:3282`); it needs a public accessor to
  iterate.
- **Locations must be materialised.** Because Zed rejects URI-only symbols (fact 2), each returned
  library symbol needs a real `file://` path and range, produced by `materialize_library_definition`
  + `deps_cache::store` (`crates/krusty-lsp/src/main.rs:387`). Each is a worker round-trip plus a
  disk write, so **cap library hits per query at 32**, ranked after project hits. Zed already
  partitions in-project vs external results, so the layer boundary matches the UI.

Dependency symbols occupy a disjoint id space from source symbols, so merging them needs no
tombstones.

### Persistence format

- One file per layer. Header: magic, format version, corpus fingerprint, section offsets, content
  checksum. Any mismatch means rebuild — never repair.
- All internal references are offsets from file start. No pointers, no serde; map and cast.
- Fixed endianness; sections aligned to 8 bytes so `u32` slice casts are sound.
- Immutable once published: write to a temp file, `fsync`, atomic `rename`.
- Mapping a file that another process may truncate or rewrite is undefined behaviour, and truncation
  raises `SIGBUS`. Mitigate by mapping only files under krusty's own cache directory, holding an
  open fd for the mapping's lifetime, and treating checksum failure as a rebuild trigger.
- `memmap2` would be a new dependency, against the project's lean posture; raw `mmap`/`munmap`
  through `extern "C"` is ~30 lines and adds none.

Note that mmap's advantage here is **not** load speed — reading 38 MiB from page cache costs
~10–20 ms, against a 5,000 ms build. The advantage is that file-backed pages are clean and
evictable, so the index stops counting as anonymous RSS.

### Compaction

Each additional segment adds a binary search to every query (k-way merge). At k ≤ 8 this stays in
microseconds. Merge deltas into a fresh baseline when segment count exceeds 8, or cumulative delta
exceeds 20% of the baseline.

Keep an LRU of baselines rather than one, so alternating between two branches hits two resident
milestones instead of thrashing. Affordable precisely because the pages are file-backed.

## Server integration

- Advertise `workspaceSymbolProvider` in the capabilities block
  (`implementation.rs:1118`). Do **not** advertise `resolveProvider`.
- Add `"workspace/symbol" => self.workspace_symbols(id, params)` to the dispatch
  (`implementation.rs:1190`).
- `EngineCommand::IndexSymbols` carries one chunk of the inventory; the build runs on the engine
  thread. It is raised alongside the diagnostic sweep, from the same `workspace_index_candidates`
  walk, once an interactive analysis has been served — enumerating a 64k-file tree ahead of the
  first open document delayed its diagnostics past two minutes.
- Its chunks are drained after interactive work and ahead of both diagnostic levels. Sharing the
  diagnostic queue would be wrong in both directions: a symbol chunk is a parse per file, and a
  diagnostic chunk is a full analysis, so one queue paces search behind a sweep that runs for hours.
- Chunks are larger than diagnostic chunks (128 files against 32) because a parse returns to the
  command loop fast enough for an interactive request to overtake it anyway.
- They sit *between* the two diagnostic levels, not ahead of both. Ahead of the sweep, which is a
  full analysis per chunk; behind the neighbourhood level, which is the fast lane for a file the
  user just saved and must not queue behind a whole project's worth of parses.
- Still to do: use `git ls-files` for discovery when the root is a git repository (0.77s versus a
  7.9s cold walk), falling back to the existing `find_sources` walker (`project/sources.rs:674`)
  otherwise.
- A query is answered inline from whatever is published. It waits only while a document is open and
  its analysis is in flight; with nothing open there is nothing to revalidate, because the project
  layer already covers the file.

### Partial results

Shards are published as they complete, and queries read whatever is published. There is no
incompleteness flag to set: `workspace/symbol` has no equivalent of `CompletionList.isIncomplete`,
and although `WorkspaceSymbolParams` extends `PartialResultParams`, Zed sends no
`partialResultToken` (`WorkspaceSymbolParams { query, ..Default::default() }`), so streaming partial
results is unavailable with this client.

The signal is therefore `$/progress`: emit periodic
`EngineEvent::Status(ServerStatus::Working("indexing symbols — N/M files"))` through the existing
work-done progress plumbing. Because Zed re-queries on every keystroke, partial results converge
within the 3–5 s build without any client-side coordination.

### Invalidation

- `didChange` / `didSave` on an open document: the next analysis batch rebuilds the live layer, and
  it shadows the project layer for that file, so no splice is needed. Watched-file changes queue a
  symbol re-index of exactly the changed files, spliced by `replace_files`.
- Project model refresh: the generation changes, the project layer is dropped, and the replacement
  sweep refills it. A batch carrying an older generation describes a model that no longer exists and
  is rejected.
- Branch switch: diff against the baseline commit, re-index the changed set into a delta segment.
- Staleness is keyed on **content hash, not mtime** — `git checkout` rewrites mtimes wholesale,
  which would invalidate everything on exactly the operation we care about. Hashing 129 MiB costs
  ~100 ms, well under the parse it saves.

## Budgets

Following the existing `MAX_*` conventions, set high enough that neither reference corpus trips
them — truncating a 700k-symbol index to save 20 MiB defeats the feature's purpose.

- max indexed files, max entries, max retained name bytes. The project layer's ceiling is separate
  from the source set's: `MAX_SOURCE_SET_WORKSPACE_SYMBOL_INDEX_WIRE_BYTES` bounds one worker
  message at 8 MiB, and inheriting it capped project-wide coverage at roughly 30k declarations.
  `MAX_PROJECT_WORKSPACE_SYMBOL_INDEX_WIRE_BYTES` is 192 MiB of wire budget, which at the ~167
  worst-case bytes this accounting charges per entry admits about 1.2M declarations against the
  698,516 the largest reference corpus holds. Admission subtracts every retained segment from that
  aggregate budget before accepting a new one; the conservative accounting includes both search
  permutations rather than relying only on the smaller serialized JSON size.
- `MAX_INDEXED_FILE_BYTES` per file, checked against metadata before the read
- `MAX_WORKSPACE_SYMBOL_RESPONSE_SYMBOLS` per response, alongside the byte ceiling
- Reaching any of them is reported to the client once per generation. A symbol the picker never
  shows is indistinguishable from one that does not exist, so silent truncation reads as absence.
- max library materialisations per query: 32
- dependency cache size cap with LRU eviction

## Testing

TDD throughout; every phase ends green on `./run-tests.sh`.

Unit:
- extractor produces identical output through the `&File` signature change
- index build, each query rung, and the ladder's fallthrough order
- wildcard prefix extraction and glob verification
- one wildcard transition budget shared across all query layers
- keyboard-layout transliteration
- adaptive qualified-vs-simple naming
- budget truncation
- aggregate project-budget enforcement before differently sized segments merge
- positional/bound phase violations preserve valid locations and report incompleteness
- metadata-rejected disk sources report incompleteness through the production adapter
- per-file invalidation splices correctly
- layer shadowing and tombstone resolution

Server-level (`crates/krusty-lsp/src/server.rs` tests):
- capabilities advertise `workspaceSymbolProvider` and not `resolveProvider`
- a query before the index is ready parks and is answered on completion
- results carry a full location with a range (never URI-only)
- progress notifications are emitted during the build

End-to-end (`crates/krusty-lsp/tests/stdio_e2e.rs`):
- real workspace, symbol found, progress observed
- library symbol resolves to a materialised `file://` path

## Phasing

Done:

- Uncapped index, ranked matching, interned names.
- Query forms: wildcards, package-qualified, wrong keyboard layout, `::` escape.
- **Project-wide, in-memory index** — URI table, a parse-only pass over the model's own source
  inventory (the same bounded ignore-aware walk the diagnostic sweep uses, rather than a second
  definition of what counts as a workspace source), an index that persists across analysis batches,
  and an open-buffer overlay so an edited file's current text wins over what is on disk.

  Two things fell out of building it. Symbol indexing gets its own queue rather than riding the
  diagnostic sweep: a symbol chunk is a parse per file and a diagnostic chunk is a full analysis, so
  sharing a queue would pace project-wide search behind a sweep that runs for hours. And binding
  URIs happens at the batch the server applies, not in the builder — the analysis worker is handed
  sources and never sees a URI, so the builder numbers entries positionally and one `assign_uris`
  call at that boundary converts positions into file ids. Nothing has to cross the worker wire.

2. **Faster discovery** — done, by walking every source root together on shared walkers rather than
   by shelling out to git. 1.6x on kotlin and 2.0x on intellij-community in the shape the callers
   actually use, no new dependency, and one definition of a workspace source.
3. **Dependency indices** — content-addressed, shared across workspaces. Immutable, so the lowest
   correctness risk of the persistence work, and the biggest cross-project payoff.
4. **mmap'd baselines and delta segments** — restart survival and evictable memory. All the format
   hazards live here (versioning, tombstones, checksums, `SIGBUS`), so it lands last, designed
   against a working index rather than a projected one.

The measurements argue against pulling 4 earlier: an in-memory delta after a branch switch is ~15 ms,
so persistence is a startup optimisation, not a branch-switching one.

## Open questions

- Whether to file the Zed upstream patch (server-declared "do not re-filter") before or after
  shipping the `::` escape.
- Whether `by_qualified` earns its 1.4 MiB, or whether qualified queries should filter `by_name`
  results by container instead.
- Whether Java sources belong in the index alongside Kotlin, given the reference corpora hold 80,622
  `.java` files in intellij-community alone.
