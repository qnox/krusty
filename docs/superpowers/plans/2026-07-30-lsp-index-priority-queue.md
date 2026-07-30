# LSP Index Priority Queue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the analysis engine a background index job class that always yields to interactive work, so a workspace sweep can run without delaying diagnostics for open documents.

**Architecture:** `CommandState` gains two extra queues behind the existing interactive `pending` deque — `neighborhood` and `sweep`. `CommandReceiver::recv` drains them strictly in that order, so an interactive command never waits behind indexing. Index work is submitted as `EngineCommand::Index(IndexJob)`, split into bounded chunks at enqueue time, and answered with `EngineEvent::IndexProgress(IndexBatch)`. This plan builds the plumbing and the ordering guarantees only; enumerating workspace sources, storing the diagnostics, and serving them over the protocol are separate plans.

**Tech Stack:** Rust 2021, `std` only. No new dependencies — the project is deliberately dependency-lean.

## Global Constraints

- The AST/IR and any index stay **index-based**: `u32` ids into parallel `Vec`s. No `Box`/`Rc` graphs.
- No `eprintln!`/`println!`/`dbg!` in compiler or LSP code. Debug output goes through `trace_compiler!("<category>", …)` from `src/trace.rs`, gated by the `trace` cargo feature.
- No logging crate (`tracing`/`log`) may be added.
- TDD is required. Every step that adds behaviour has a test written and watched failing first.
- No AI/assistant/tool attribution anywhere: no `Co-Authored-By` for tools, no "Generated with", no 🤖, in commits, PR bodies, code comments, or docs.
- Validate with `./run-tests.sh`, not bare `cargo test`. Set `JAVA_HOME` first or the JVM box tests fail:
  `JAVA_HOME=$(/usr/libexec/java_home) ./run-tests.sh`. For a focused run during the cycle,
  `cargo test -p krusty-lsp --profile gate --lib -- <filter>` is fine.
- Do not use `--release` for tests.

## File Structure

| File | Responsibility | Change |
| --- | --- | --- |
| `crates/krusty-lsp/src/lib.rs` | Shared analysis result types (`DocumentAnalysis`, `MaterializedDefinition`) | Add `IndexedFile` |
| `crates/krusty-lsp/src/server/engine.rs` | Command queue, engine thread, engine events | Add `IndexPriority`, `IndexJob`, `IndexBatch`, `EngineCommand::Index`, `EngineEvent::IndexProgress`, the two priority queues, chunking and dedup |
| `crates/krusty-lsp/src/server/implementation.rs` | `Analysis` trait, `AnalysisBackend`, `LspService` | Add the defaulted `Analysis::index_workspace_files` method |

`engine.rs` is ~1100 lines and already owns queueing, so the queues belong there. `IndexedFile` goes in `lib.rs` beside `DocumentAnalysis` because both are worker results crossing the same boundary, and `engine.rs` already imports from there.

---

### Task 1: Index job types and the trait seam

**Files:**
- Modify: `crates/krusty-lsp/src/lib.rs`
- Modify: `crates/krusty-lsp/src/server/implementation.rs` (the `Analysis` trait)
- Modify: `crates/krusty-lsp/src/server/engine.rs`
- Test: `crates/krusty-lsp/src/server/engine.rs` (`mod tests` at the bottom)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `IndexedFile { uri: String, diagnostics: Vec<krusty::diag::Diagnostic>, text_hash: u64 }`;
  `IndexPriority { Neighborhood, Sweep }`;
  `IndexJob { priority: IndexPriority, uris: Vec<String> }`;
  `IndexBatch { priority: IndexPriority, files: Vec<IndexedFile>, remaining: usize }`;
  `EngineCommand::Index(IndexJob)`; `EngineEvent::IndexProgress(IndexBatch)`;
  `Analysis::index_workspace_files(&mut self, uris: &[&str]) -> Vec<IndexedFile>`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at the bottom of `crates/krusty-lsp/src/server/engine.rs`:

```rust
    #[test]
    fn an_index_command_produces_a_progress_event() {
        use std::sync::mpsc::sync_channel;

        struct Mock;
        impl Analysis for Mock {
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }

            fn index_workspace_files(&mut self, uris: &[&str]) -> Vec<IndexedFile> {
                uris.iter()
                    .map(|uri| IndexedFile {
                        uri: (*uri).to_string(),
                        diagnostics: Vec::new(),
                        text_hash: 7,
                    })
                    .collect()
            }
        }

        let (tx, rx) = sync_channel(8);
        let engine = AnalysisEngine::spawn(Mock, tx);
        engine.submit(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/A.kt".into(), "file:///w/B.kt".into()],
        }));

        let batch = (0..8).find_map(|_| match rx.recv().unwrap() {
            Incoming::Engine(EngineEvent::IndexProgress(batch)) => Some(batch),
            _ => None,
        });
        let batch = batch.expect("index progress event");
        assert_eq!(batch.priority, IndexPriority::Sweep);
        assert_eq!(
            batch.files.iter().map(|f| f.uri.as_str()).collect::<Vec<_>>(),
            vec!["file:///w/A.kt", "file:///w/B.kt"]
        );
        assert_eq!(batch.files[0].text_hash, 7);
        assert_eq!(batch.remaining, 0);
        engine.join();
    }
```

- [ ] **Step 2: Run the test and watch it fail**

Run: `cargo test -p krusty-lsp --profile gate --lib -- an_index_command_produces_a_progress_event`

Expected: compile error — `cannot find type IndexedFile`, `no variant Index`, `no variant IndexProgress`.

- [ ] **Step 3: Add `IndexedFile` to `lib.rs`**

Place it next to `DocumentAnalysis`:

```rust
/// One workspace file's indexing result. Only diagnostics and the text hash are retained; the
/// rich per-document indices are derived during indexing and dropped, because a swept file is
/// re-analysed interactively the moment it is opened.
#[derive(Clone, Debug)]
pub struct IndexedFile {
    pub uri: String,
    pub diagnostics: Vec<krusty::diag::Diagnostic>,
    pub text_hash: u64,
}
```

- [ ] **Step 4: Add the trait method in `implementation.rs`**

In the `Analysis` trait, alongside the other defaulted methods:

```rust
    /// Index workspace files that are not open. Defaults to doing nothing so that existing
    /// implementations, including every test mock, keep compiling unchanged.
    fn index_workspace_files(&mut self, _uris: &[&str]) -> Vec<IndexedFile> {
        Vec::new()
    }
```

Add `IndexedFile` to the existing `use super::super::{…}` import at the top of the file.

- [ ] **Step 5: Add the engine types in `engine.rs`**

Extend the import at the top:

```rust
use super::super::{DocumentAnalysis, IndexedFile, MaterializedDefinition};
```

Then add, next to `AnalysisJob`:

```rust
/// Index levels, ordered strictly behind interactive work and behind each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexPriority {
    /// Files in modules that contain an open document, or that depend on one.
    Neighborhood,
    /// Everything else in the workspace.
    Sweep,
}

#[derive(Debug)]
pub struct IndexJob {
    pub priority: IndexPriority,
    pub uris: Vec<String>,
}

pub struct IndexBatch {
    pub priority: IndexPriority,
    pub files: Vec<IndexedFile>,
    /// Chunks of the same priority still queued behind this one.
    pub remaining: usize,
}
```

Add the command and event variants:

```rust
pub(crate) enum EngineCommand {
    SetWorkspaceRoot(Option<std::path::PathBuf>),
    Analyze(AnalysisJob),
    Materialize(MaterializeJob),
    Index(IndexJob),
    ProjectChange {
        refresh: bool,
        reanalyze: bool,
        uris: Vec<String>,
    },
}
```

```rust
pub(crate) enum EngineEvent {
    ReadyState(bool),
    WatchedGlobs(Vec<String>),
    Project(ProjectFeedback),
    ReanalyzeRequested,
    AnalysisComplete(AnalysisBatch),
    IndexProgress(IndexBatch),
    Materialized(MaterializeResult),
    Status(ServerStatus),
}
```

- [ ] **Step 6: Handle the command in `run`**

In the `match command` block of `run`, add an arm before the `ProjectChange` arm:

```rust
            Some(EngineCommand::Index(job)) => {
                let uris: Vec<&str> = job.uris.iter().map(String::as_str).collect();
                let files = analyze.index_workspace_files(&uris);
                let remaining = commands.queued_index_chunks(job.priority);
                if events
                    .send(Incoming::Engine(EngineEvent::IndexProgress(IndexBatch {
                        priority: job.priority,
                        files,
                        remaining,
                    })))
                    .is_err()
                {
                    break;
                }
            }
```

`queued_index_chunks` does not exist yet. For this task only, add a temporary implementation on `CommandReceiver` that always reports zero; Task 2 replaces it with the real count:

```rust
impl CommandReceiver {
    fn queued_index_chunks(&self, _priority: IndexPriority) -> usize {
        0
    }
}
```

- [ ] **Step 7: Handle the new variant in `CommandState::enqueue`**

Add a temporary arm so the match compiles; Task 2 replaces it:

```rust
            EngineCommand::Index(job) => {
                self.pending.push_back(EngineCommand::Index(job));
            }
```

- [ ] **Step 8: Run the test and watch it pass**

Run: `cargo test -p krusty-lsp --profile gate --lib -- an_index_command_produces_a_progress_event`

Expected: PASS, 1 passed.

- [ ] **Step 9: Run the whole LSP suite**

Run: `cargo test -p krusty-lsp --profile gate`

Expected: every test passes. The defaulted trait method means no existing mock needs changing.

- [ ] **Step 10: Commit**

```bash
git add crates/krusty-lsp/src/lib.rs crates/krusty-lsp/src/server/engine.rs crates/krusty-lsp/src/server/implementation.rs
git commit -m "feat(lsp): add an index job class to the analysis engine

Workspace indexing needs a command the engine can run without holding up
open-document analysis. This adds the job, batch, and event types plus a
defaulted Analysis::index_workspace_files seam, so existing analyses keep
compiling while the queueing that makes it yield lands next."
```

---

### Task 2: Priority queues that always yield to interactive work

**Files:**
- Modify: `crates/krusty-lsp/src/server/engine.rs:130-360` (`CommandState`, `CommandSender`, `CommandReceiver`)
- Test: `crates/krusty-lsp/src/server/engine.rs` (`mod tests`)

**Interfaces:**
- Consumes: `IndexPriority`, `IndexJob`, `EngineCommand::Index` from Task 1.
- Produces: `CommandState { pending, neighborhood, sweep, queued, disconnected }`;
  `CommandReceiver::queued_index_chunks(&self, priority: IndexPriority) -> usize` returning the real count.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn interactive_commands_are_served_before_queued_index_chunks() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/Swept.kt".into()],
        }));
        state.enqueue(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///w/Open.kt".into(), String::new(), 1)],
            open_uris: vec!["file:///w/Open.kt".into()],
        }));

        assert!(
            matches!(state.take(), Some(EngineCommand::Analyze(_))),
            "an open document must never wait behind a sweep chunk"
        );
        assert!(matches!(state.take(), Some(EngineCommand::Index(_))));
        assert!(state.take().is_none());
    }

    #[test]
    fn neighborhood_chunks_are_served_before_sweep_chunks() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/Far.kt".into()],
        }));
        state.enqueue(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Neighborhood,
            uris: vec!["file:///w/Near.kt".into()],
        }));

        let Some(EngineCommand::Index(first)) = state.take() else {
            panic!("expected an index chunk");
        };
        assert_eq!(first.priority, IndexPriority::Neighborhood);
        let Some(EngineCommand::Index(second)) = state.take() else {
            panic!("expected an index chunk");
        };
        assert_eq!(second.priority, IndexPriority::Sweep);
    }

    #[test]
    fn queued_index_chunks_counts_only_the_same_priority() {
        let mut state = CommandState::default();
        for index in 0..3 {
            state.enqueue(EngineCommand::Index(IndexJob {
                priority: IndexPriority::Sweep,
                uris: vec![format!("file:///w/S{index}.kt")],
            }));
        }
        state.enqueue(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Neighborhood,
            uris: vec!["file:///w/N.kt".into()],
        }));

        assert_eq!(state.queued_index_chunks(IndexPriority::Sweep), 3);
        assert_eq!(state.queued_index_chunks(IndexPriority::Neighborhood), 1);
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p krusty-lsp --profile gate --lib -- interactive_commands_are_served_before neighborhood_chunks_are_served queued_index_chunks_counts_only`

Expected: compile error — `no method named take`, `no method named queued_index_chunks` on `CommandState`.

- [ ] **Step 3: Give `CommandState` the extra queues**

Replace the struct and add `take`:

```rust
#[derive(Default)]
struct CommandState {
    pending: VecDeque<EngineCommand>,
    neighborhood: VecDeque<IndexJob>,
    sweep: VecDeque<IndexJob>,
    disconnected: bool,
}

impl CommandState {
    /// Interactive work first, then the neighbourhood, then the sweep. The levels are the
    /// priority, so there is no comparator and no heap.
    fn take(&mut self) -> Option<EngineCommand> {
        if let Some(command) = self.pending.pop_front() {
            return Some(command);
        }
        if let Some(job) = self.neighborhood.pop_front() {
            return Some(EngineCommand::Index(job));
        }
        self.sweep.pop_front().map(EngineCommand::Index)
    }

    fn queued_index_chunks(&self, priority: IndexPriority) -> usize {
        match priority {
            IndexPriority::Neighborhood => self.neighborhood.len(),
            IndexPriority::Sweep => self.sweep.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.neighborhood.is_empty() && self.sweep.is_empty()
    }
}
```

- [ ] **Step 4: Route `Index` into the right queue**

Replace the temporary arm from Task 1 Step 7:

```rust
            EngineCommand::Index(job) => match job.priority {
                IndexPriority::Neighborhood => self.neighborhood.push_back(job),
                IndexPriority::Sweep => self.sweep.push_back(job),
            },
```

- [ ] **Step 5: Make `recv` use `take`**

In `CommandReceiver::recv`, replace `if let Some(command) = state.pending.pop_front()` with:

```rust
            if let Some(command) = state.take() {
                return CommandReceive::Command(command);
            }
```

and replace the `state.pending.is_empty()` check in the timeout branch with `state.is_empty()`.

- [ ] **Step 6: Give the receiver the real chunk count**

Delete the temporary `queued_index_chunks` from Task 1 Step 6 and replace it with:

```rust
impl CommandReceiver {
    fn queued_index_chunks(&self, priority: IndexPriority) -> usize {
        self.queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .queued_index_chunks(priority)
    }
}
```

- [ ] **Step 7: Run the tests and watch them pass**

Run: `cargo test -p krusty-lsp --profile gate --lib -- interactive_commands_are_served_before neighborhood_chunks_are_served queued_index_chunks_counts_only`

Expected: PASS, 3 passed.

- [ ] **Step 8: Run the whole LSP suite**

Run: `cargo test -p krusty-lsp --profile gate`

Expected: all pass. `command_queue_bounds_project_change_bursts`,
`workspace_reconfiguration_stays_before_pending_analysis`, and
`command_queue_replaces_obsolete_analysis_without_growing` still exercise `pending` only and are
unaffected.

- [ ] **Step 9: Commit**

```bash
git add crates/krusty-lsp/src/server/engine.rs
git commit -m "feat(lsp): serve index chunks behind all interactive commands

Index work now sits in its own queues, drained only once the interactive
deque is empty and neighbourhood before sweep. An open document's analysis
can therefore never wait behind a workspace chunk."
```

---

### Task 3: Bounded chunks and enqueue-if-absent

**Files:**
- Modify: `crates/krusty-lsp/src/server/engine.rs` (`CommandState::enqueue`, new constants)
- Test: `crates/krusty-lsp/src/server/engine.rs` (`mod tests`)

**Interfaces:**
- Consumes: the queues from Task 2.
- Produces: `MAX_INDEX_CHUNK_FILES: usize = 32`; `MAX_QUEUED_INDEX_FILES: usize = 200_000`;
  `CommandState.queued: HashSet<String>` guarding duplicate enqueues.

**Why 32:** a chunk is the longest an interactive command can be made to wait. The worker batches
sources up to `crate::worker::MAX_SOURCE_SET_BYTES` (32 MiB); 32 files of typical Kotlin source sit
well inside that, so a chunk is one worker round trip.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_large_index_job_is_split_into_bounded_chunks() {
        let mut state = CommandState::default();
        let uris: Vec<String> = (0..(MAX_INDEX_CHUNK_FILES * 2 + 1))
            .map(|index| format!("file:///w/F{index}.kt"))
            .collect();
        let total = uris.len();
        state.enqueue(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris,
        }));

        assert_eq!(state.queued_index_chunks(IndexPriority::Sweep), 3);
        let mut seen = 0;
        while let Some(EngineCommand::Index(job)) = state.take() {
            assert!(
                job.uris.len() <= MAX_INDEX_CHUNK_FILES,
                "no chunk may exceed the bound that caps interactive latency"
            );
            seen += job.uris.len();
        }
        assert_eq!(seen, total);
    }

    #[test]
    fn a_file_already_queued_is_not_queued_again() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/A.kt".into(), "file:///w/B.kt".into()],
        }));
        state.enqueue(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/B.kt".into(), "file:///w/C.kt".into()],
        }));

        let mut queued = Vec::new();
        while let Some(EngineCommand::Index(job)) = state.take() {
            queued.extend(job.uris);
        }
        queued.sort();
        assert_eq!(queued, vec!["file:///w/A.kt", "file:///w/B.kt", "file:///w/C.kt"]);
    }

    #[test]
    fn taking_a_chunk_releases_its_files_for_requeueing() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/A.kt".into()],
        }));
        assert!(matches!(state.take(), Some(EngineCommand::Index(_))));

        state.enqueue(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/A.kt".into()],
        }));
        let Some(EngineCommand::Index(job)) = state.take() else {
            panic!("a file that finished indexing must be requeueable when it changes");
        };
        assert_eq!(job.uris, vec!["file:///w/A.kt".to_string()]);
    }

    #[test]
    fn the_index_queue_stops_growing_at_its_bound() {
        let mut state = CommandState::default();
        let uris: Vec<String> = (0..(MAX_QUEUED_INDEX_FILES + 10))
            .map(|index| format!("file:///w/F{index}.kt"))
            .collect();
        state.enqueue(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris,
        }));

        let mut queued = 0;
        while let Some(EngineCommand::Index(job)) = state.take() {
            queued += job.uris.len();
        }
        assert_eq!(queued, MAX_QUEUED_INDEX_FILES);
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p krusty-lsp --profile gate --lib -- a_large_index_job_is_split a_file_already_queued taking_a_chunk_releases the_index_queue_stops_growing`

Expected: compile error — `cannot find value MAX_INDEX_CHUNK_FILES`; the dedup and bound tests fail
on the assertions once the constants exist.

- [ ] **Step 3: Add the constants and the guard set**

Next to `MAX_PENDING_WATCHED_FILES` at the top of `engine.rs`:

```rust
/// The longest an interactive command can be made to wait: one chunk of index work. Sized to sit
/// inside a single worker source-set round trip.
const MAX_INDEX_CHUNK_FILES: usize = 32;
/// Ceiling on files awaiting indexing, so a pathological workspace cannot grow the queue without
/// bound. Reaching it drops the excess; the sweep re-offers those files on its next pass.
const MAX_QUEUED_INDEX_FILES: usize = 200_000;
```

Add the guard set to the struct (and `use std::collections::HashSet;` at the top):

```rust
#[derive(Default)]
struct CommandState {
    pending: VecDeque<EngineCommand>,
    neighborhood: VecDeque<IndexJob>,
    sweep: VecDeque<IndexJob>,
    queued: HashSet<String>,
    disconnected: bool,
}
```

- [ ] **Step 4: Split and dedup on enqueue**

Replace the `EngineCommand::Index` arm from Task 2 Step 4:

```rust
            EngineCommand::Index(job) => {
                let priority = job.priority;
                let mut chunk = Vec::with_capacity(MAX_INDEX_CHUNK_FILES.min(job.uris.len()));
                for uri in job.uris {
                    if self.queued.len() >= MAX_QUEUED_INDEX_FILES {
                        break;
                    }
                    if !self.queued.insert(uri.clone()) {
                        continue;
                    }
                    chunk.push(uri);
                    if chunk.len() == MAX_INDEX_CHUNK_FILES {
                        self.push_index_chunk(priority, std::mem::take(&mut chunk));
                        chunk.reserve(MAX_INDEX_CHUNK_FILES);
                    }
                }
                if !chunk.is_empty() {
                    self.push_index_chunk(priority, chunk);
                }
            }
```

Add the helper to `impl CommandState`:

```rust
    fn push_index_chunk(&mut self, priority: IndexPriority, uris: Vec<String>) {
        let job = IndexJob { priority, uris };
        match priority {
            IndexPriority::Neighborhood => self.neighborhood.push_back(job),
            IndexPriority::Sweep => self.sweep.push_back(job),
        }
    }
```

- [ ] **Step 5: Release files from the guard set when a chunk is taken**

In `CommandState::take`, replace the two index branches so a taken chunk stops blocking requeues:

```rust
        if let Some(job) = self.neighborhood.pop_front() {
            self.release(&job.uris);
            return Some(EngineCommand::Index(job));
        }
        let job = self.sweep.pop_front()?;
        self.release(&job.uris);
        Some(EngineCommand::Index(job))
```

and add the helper to the same `impl CommandState`:

```rust
    fn release(&mut self, uris: &[String]) {
        for uri in uris {
            self.queued.remove(uri);
        }
    }
```

- [ ] **Step 6: Run the tests and watch them pass**

Run: `cargo test -p krusty-lsp --profile gate --lib -- a_large_index_job_is_split a_file_already_queued taking_a_chunk_releases the_index_queue_stops_growing`

Expected: PASS, 4 passed.

- [ ] **Step 7: Run the whole LSP suite**

Run: `cargo test -p krusty-lsp --profile gate`

Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/krusty-lsp/src/server/engine.rs
git commit -m "feat(lsp): bound index chunks and skip files already queued

A chunk is the longest an interactive command can be made to wait, so jobs
are split at a fixed file count rather than submitted whole. Enqueueing the
same file twice is a no-op while it is waiting, and the queue stops growing
at a ceiling so a pathological workspace cannot exhaust memory."
```

---

### Task 4: Status reporting and shutdown behaviour

**Files:**
- Modify: `crates/krusty-lsp/src/server/engine.rs` (the `working` match in `run`, `AnalysisEngine::submit`)
- Test: `crates/krusty-lsp/src/server/engine.rs` (`mod tests`)

**Interfaces:**
- Consumes: everything from Tasks 1-3.
- Produces: no new public names. `ServerStatus::Working("Indexing N files")` is emitted for index
  chunks, and a disconnected engine drops queued index work instead of draining it.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn index_chunks_report_progress_as_working_status() {
        use std::sync::mpsc::sync_channel;
        use std::time::Duration;

        struct Mock;
        impl Analysis for Mock {
            fn analyze(&mut self, s: &[&str]) -> Vec<DocumentAnalysis> {
                s.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
            fn index_workspace_files(&mut self, uris: &[&str]) -> Vec<IndexedFile> {
                uris.iter()
                    .map(|uri| IndexedFile {
                        uri: (*uri).to_string(),
                        diagnostics: Vec::new(),
                        text_hash: 0,
                    })
                    .collect()
            }
        }

        let (tx, rx) = sync_channel(16);
        let engine = AnalysisEngine::spawn(Mock, tx);
        engine.submit(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/A.kt".into(), "file:///w/B.kt".into()],
        }));

        let mut statuses = Vec::new();
        while statuses.len() < 2 {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Incoming::Engine(EngineEvent::Status(status))) => statuses.push(status),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert_eq!(
            statuses,
            vec![
                ServerStatus::Working("Indexing 2 files".to_string()),
                ServerStatus::Ready,
            ]
        );
        engine.join();
    }

    #[test]
    fn a_disconnected_queue_abandons_index_work_but_finishes_interactive_work() {
        let (sender, receiver) = command_queue();
        sender.send(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///w/Open.kt".into(), String::new(), 1)],
            open_uris: vec!["file:///w/Open.kt".into()],
        }));
        sender.send(EngineCommand::Index(IndexJob {
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/Swept.kt".into()],
        }));
        sender.disconnect();

        assert!(
            matches!(
                receiver.recv(None),
                CommandReceive::Command(EngineCommand::Analyze(_))
            ),
            "interactive work already queued still completes across a shutdown"
        );
        assert!(
            matches!(receiver.recv(None), CommandReceive::Disconnected),
            "queued sweep work is abandoned rather than drained, so exit stays prompt"
        );
    }

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p krusty-lsp --profile gate --lib -- index_chunks_report_progress a_disconnected_queue_abandons`

Expected: the status test fails — no `Working` status is emitted for index commands, so `statuses`
holds fewer than two entries and the assertion reports a mismatch.

- [ ] **Step 3: Report status for index chunks**

In `run`, extend the `working` match:

```rust
        let working = match &command {
            None => Some("Refreshing project".to_string()),
            Some(EngineCommand::SetWorkspaceRoot(_)) => Some("Loading project".to_string()),
            Some(EngineCommand::Analyze(job)) => {
                Some(format!("Analyzing {} files", job.documents.len()))
            }
            Some(EngineCommand::Index(job)) => {
                Some(format!("Indexing {} files", job.uris.len()))
            }
            _ => None,
        };
```

In the `EngineCommand::Index` arm added in Task 1 Step 6, send `ServerStatus::Ready` after the
progress event, matching the `Analyze` arm:

```rust
                if send_status(&events, ServerStatus::Ready).is_err() {
                    break;
                }
```

- [ ] **Step 4: Drop queued index work on disconnect**

In `CommandReceiver::recv`, check for disconnection before taking index work so a shutdown does not
have to drain the sweep. Replace the take-then-check order with:

```rust
            if let Some(command) = state.pending.pop_front() {
                return CommandReceive::Command(command);
            }
            if state.disconnected {
                return CommandReceive::Disconnected;
            }
            if let Some(command) = state.take() {
                return CommandReceive::Command(command);
            }
```

`take` still drains `pending` first; the earlier `pop_front` only makes the interactive queue win
over the disconnect check, so in-flight interactive work still completes.

- [ ] **Step 5: Run the tests and watch them pass**

Run: `cargo test -p krusty-lsp --profile gate --lib -- index_chunks_report_progress a_disconnected_queue_abandons`

Expected: PASS, 2 passed.

- [ ] **Step 6: Run the whole LSP suite**

Run: `cargo test -p krusty-lsp --profile gate`

Expected: all pass, including `run_loop_reports_project_and_analysis_work` and
`join_terminates_the_thread_without_a_prior_shutdown_command`.

- [ ] **Step 7: Run the full harness**

Run: `JAVA_HOME=$(/usr/libexec/java_home) ./run-tests.sh`

Expected: `all test binaries passed`. If JVM box tests fail with
`kotlinc(lib) failed (1): error: no source files ... krusty_scratch/<pid>/…`, that is scratch-dir
contention from other concurrent sessions on the machine, not this change — re-run the named
failures in isolation to confirm before reporting.

- [ ] **Step 8: Commit**

```bash
git add crates/krusty-lsp/src/server/engine.rs
git commit -m "feat(lsp): report indexing progress and drop it on shutdown

Index chunks now raise the same Working/Ready status pair as analysis, so a
sweep is visible rather than silent. Shutdown abandons whatever sweep work is
still queued instead of draining it, which on a large workspace would other-
wise delay exit by minutes."
```

---

## Follow-on plans

This plan deliberately stops at the queueing. The remaining Part 2 subsystems each get their own
plan, in this order, because each depends on the one before:

1. **Workspace source enumeration and the diagnostic store** — walk the project model's source
   roots, feed `EngineCommand::Index`, and retain results in the CSR-backed store with the global
   message interner. Depends on this plan's `IndexProgress` event.
2. **Reverse dependency index** — record `referencing file -> declaring file` edges during
   indexing, hash each file's exported signature surface, and requeue dependents at
   `Neighborhood` priority only when that hash moves. Depends on plan 1's `FileId` interning.
3. **Workspace diagnostic protocol** — advertise `workspaceDiagnostics: true`, stream
   `workspace/diagnostic` through `partialResultToken`, and fall back to push per completed chunk.
   Depends on plan 1's store for the report contents.
