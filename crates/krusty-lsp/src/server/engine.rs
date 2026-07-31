//! Analysis worker for the LSP request loop.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::super::{
    workspace_index_uri_bytes, DocumentAnalysis, IndexedFile, MaterializedDefinition,
    MAX_WORKSPACE_INDEX_FILES,
};
use super::implementation::{
    Analysis, AnalysisBackend, DocumentAdmission, Incoming, ProjectFeedback,
};
use crate::compiler_analysis::LibraryRef;

const MAX_PENDING_WATCHED_FILES: usize = 1024;
/// The longest an interactive command can be made to wait: one chunk of index work. Sized to sit
/// inside a single worker source-set round trip.
const MAX_INDEX_CHUNK_FILES: usize = 32;
/// Ceiling on files awaiting indexing, so a pathological workspace cannot grow the queue without
/// bound. Reaching it drops the excess and marks this generation incomplete for the client log.
const MAX_QUEUED_INDEX_FILES: usize = MAX_WORKSPACE_INDEX_FILES;
/// Companion byte ceiling. A count alone is not a memory bound: deeply nested workspaces produce
/// long URIs, and promotion can retain three copies across both priority chunks and the map.
const MAX_QUEUED_INDEX_BYTES: usize = 32 * 1024 * 1024;

fn queued_uri_bytes(uri: &str) -> usize {
    // A queued URI owns the map key and one chunk string. Promotion can temporarily leave the old
    // sweep string beside a new neighbourhood string, so reserve for all three representations up
    // front; releasing that reservation with the promoted claim still leaves at most its one-third
    // stale copy while newly admitted work consumes the rest of the ceiling.
    workspace_index_uri_bytes(uri).saturating_mul(3)
}

#[derive(Clone, Copy)]
struct QueuedIndexEntry {
    priority: IndexPriority,
    /// Promotion queues a neighbourhood copy ahead of the original sweep copy. The latter remains
    /// in its chunk until that chunk reaches the front, so its reserved bytes cannot be released
    /// when the promoted copy is claimed.
    stale_sweep_copy: bool,
}

/// Index levels, ordered strictly behind interactive work and behind each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexPriority {
    /// Files in modules that hold an open document, or that depend on one.
    Neighborhood,
    /// Everything else in the workspace.
    Sweep,
}

#[derive(Debug)]
pub struct IndexJob {
    /// Project-model generation this work was queued under. Results carrying a stale generation
    /// describe a model that no longer exists and are rejected rather than stored.
    pub generation: u64,
    pub priority: IndexPriority,
    pub uris: Vec<String>,
}

pub struct IndexBatch {
    pub generation: u64,
    /// Every URI the chunk attempted, in submission order. A URI absent from `files` was deleted,
    /// unreadable, or rejected, and its retained data must be removed rather than left stale.
    pub attempted: Vec<String>,
    pub files: Vec<IndexedFile>,
    /// False when the analysis could not run at all, so `attempted` must not be read as deletions.
    pub conclusive: bool,
}

#[derive(Debug)]
pub struct AnalysisJob {
    pub documents: Vec<(String, String, i64)>,
    pub open_uris: Vec<String>,
}

pub struct AnalysisBatch {
    pub analyzed: Vec<(String, i64)>,
    pub analyses: Vec<DocumentAnalysis>,
    pub support_documents: Vec<(String, String)>,
    pub pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ServerStatus {
    Working(String),
    Ready,
}

#[derive(Debug)]
pub struct MaterializeJob {
    pub token: u64,
    pub reference: LibraryRef,
}

pub struct MaterializeResult {
    pub token: u64,
    pub definition: Option<MaterializedDefinition>,
}

/// Where a dev-mode dump was written.
///
/// Only the path travels: the rendered document carries every AST node, typed expression, and IR
/// instruction of a file, so the code action navigates to it rather than inlining it into a
/// response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DumpResult {
    pub path: std::path::PathBuf,
}

/// A dump request on its way to the analysis thread. `token` correlates it with the LSP request id
/// the session parked while waiting.
#[derive(Debug)]
pub struct DumpJob {
    pub token: u64,
    pub uri: String,
}

pub struct DumpOutcome {
    pub token: u64,
    /// `None` when the document is not dumpable — dev mode is off, the document was not part of the
    /// retained analysis payload, or rendering failed.
    pub dump: Option<DumpResult>,
}

#[derive(Debug)]
pub(crate) enum EngineCommand {
    SetWorkspaceRoot(Option<std::path::PathBuf>),
    Analyze(AnalysisJob),
    Materialize(MaterializeJob),
    Dump(DumpJob),
    Index(IndexJob),
    ProjectChange {
        refresh: bool,
        reanalyze: bool,
        uris: Vec<String>,
    },
}

pub(crate) enum EngineEvent {
    ReadyState(bool),
    WatchedGlobs(Vec<String>),
    Project(ProjectFeedback),
    ReanalyzeRequested,
    AnalysisComplete(AnalysisBatch),
    /// The project model or its compiler configuration changed. Clear retained workspace results
    /// immediately; waiting for the first batch of the replacement sweep would expose old-model
    /// diagnostics in the interval.
    IndexReset(u64),
    IndexProgress(IndexBatch),
    Materialized(MaterializeResult),
    Dumped(DumpOutcome),
    Status(ServerStatus),
}

pub(crate) struct AnalysisEngine {
    commands: Option<CommandSender>,
    handle: Option<JoinHandle<()>>,
    admission: Arc<RwLock<DocumentAdmission>>,
}

impl AnalysisEngine {
    pub(crate) fn spawn<A: Analysis + Send + 'static>(
        analyze: A,
        events: SyncSender<Incoming>,
    ) -> AnalysisEngine {
        let (commands, command_rx) = command_queue();
        let admission = Arc::new(RwLock::new(DocumentAdmission::default()));
        let engine_admission = admission.clone();
        let handle = std::thread::spawn(move || run(analyze, command_rx, events, engine_admission));
        AnalysisEngine {
            commands: Some(commands),
            handle: Some(handle),
            admission,
        }
    }

    pub(crate) fn submit(&self, command: EngineCommand) {
        if let Some(commands) = self.commands.as_ref() {
            commands.send(command);
        }
    }

    pub(crate) fn disconnect(&mut self) {
        if let Some(commands) = self.commands.take() {
            commands.disconnect();
        }
    }

    pub(crate) fn join(mut self) {
        self.disconnect();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    /// Detach a thread that has not answered `disconnect`, so shutdown can proceed
    /// without waiting on it. Process teardown reaps the thread.
    pub(crate) fn abandon(mut self) {
        self.disconnect();
        drop(self.handle.take());
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn accepts_document_set(&self, documents: &[(&str, usize)]) -> bool {
        self.admission
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .accepts(documents)
    }
}

struct CommandSender {
    queue: Arc<CommandQueue>,
}

struct CommandReceiver {
    queue: Arc<CommandQueue>,
}

struct CommandQueue {
    state: Mutex<CommandState>,
    ready: Condvar,
}

#[derive(Default)]
struct CommandState {
    pending: VecDeque<EngineCommand>,
    neighborhood: VecDeque<IndexJob>,
    sweep: VecDeque<IndexJob>,
    /// The level each queued URI currently belongs to. Promotion rewrites the level here and
    /// queues the URI again; the superseded entry is filtered out when its chunk is handed out.
    queued: HashMap<String, QueuedIndexEntry>,
    queued_bytes: usize,
    index_admission_truncated: bool,
    /// Files handed out, and files enumerated, since the sweep began. Progress reports the pair so
    /// a large workspace shows movement instead of a constant chunk size.
    indexed_done: usize,
    indexed_total: usize,
    generation: u64,
    disconnected: bool,
}

enum CommandReceive {
    Command(EngineCommand),
    Timeout,
    Disconnected,
}

fn command_queue() -> (CommandSender, CommandReceiver) {
    let queue = Arc::new(CommandQueue {
        state: Mutex::new(CommandState::default()),
        ready: Condvar::new(),
    });
    (
        CommandSender {
            queue: queue.clone(),
        },
        CommandReceiver { queue },
    )
}

impl CommandSender {
    fn send(&self, command: EngineCommand) {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.disconnected {
            return;
        }
        state.enqueue(command);
        self.queue.ready.notify_one();
    }

    fn disconnect(self) {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.disconnected = true;
        self.queue.ready.notify_one();
    }
}

impl CommandState {
    fn enqueue(&mut self, command: EngineCommand) {
        match command {
            EngineCommand::Analyze(job) => {
                if let Some(index) = self
                    .pending
                    .iter()
                    .rposition(|command| matches!(command, EngineCommand::Analyze(_)))
                {
                    self.pending.remove(index);
                }
                self.compact_project_changes();
                self.pending.push_back(EngineCommand::Analyze(job));
            }
            EngineCommand::Materialize(job) => {
                self.pending.push_back(EngineCommand::Materialize(job));
            }
            // Appended in arrival order, which is the whole guarantee: a dump does NOT wait for a
            // fresher analysis. `Analyze` coalescing drops the pending job and pushes its
            // replacement at the back, so an edit arriving after this dump is analyzed after it.
            // A dump therefore replays whatever payload the last completed pass retained, which can
            // predate the buffer on screen; the source hash in the document's header is what tells
            // the reader which text it was rendered from.
            EngineCommand::Dump(job) => {
                self.pending.push_back(EngineCommand::Dump(job));
            }
            EngineCommand::Index(job) => {
                let priority = job.priority;
                let mut chunk = Vec::with_capacity(MAX_INDEX_CHUNK_FILES.min(job.uris.len()));
                for uri in job.uris {
                    match self.queued.get_mut(&uri) {
                        // Already waiting at this level or a higher one; nothing to do.
                        Some(entry) if entry.priority == IndexPriority::Neighborhood => continue,
                        Some(entry)
                            if entry.priority == IndexPriority::Sweep
                                && priority == IndexPriority::Sweep =>
                        {
                            continue;
                        }
                        // Promotion does not retain another map key, so it must remain possible even
                        // when the queue is at either admission ceiling. Initial admission reserved
                        // all three copies, including the sweep string that promotion leaves stale.
                        Some(entry) => {
                            entry.priority = priority;
                            entry.stale_sweep_copy = true;
                        }
                        None => {
                            if self.queued.len() >= MAX_QUEUED_INDEX_FILES {
                                self.index_admission_truncated = true;
                                break;
                            }
                            let retained_bytes = queued_uri_bytes(&uri);
                            if retained_bytes
                                > MAX_QUEUED_INDEX_BYTES.saturating_sub(self.queued_bytes)
                            {
                                // A later, shorter URI may still fit the remaining byte budget.
                                self.index_admission_truncated = true;
                                continue;
                            }
                            self.queued_bytes = self.queued_bytes.saturating_add(retained_bytes);
                            self.queued.insert(
                                uri.clone(),
                                QueuedIndexEntry {
                                    priority,
                                    stale_sweep_copy: false,
                                },
                            );
                            // Count admitted work, not queue copies. Promotion adds a higher-priority
                            // copy and leaves the sweep copy stale, but the URI is still processed once.
                            self.indexed_total = self.indexed_total.saturating_add(1);
                        }
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
            EngineCommand::SetWorkspaceRoot(root) => {
                self.replace_index_generation();
                let analysis = self
                    .pending
                    .iter()
                    .rposition(|command| matches!(command, EngineCommand::Analyze(_)))
                    .and_then(|index| self.pending.remove(index));
                if let Some(index) = self
                    .pending
                    .iter()
                    .rposition(|command| matches!(command, EngineCommand::SetWorkspaceRoot(_)))
                {
                    self.pending.remove(index);
                }
                self.compact_project_changes();
                self.pending
                    .push_back(EngineCommand::SetWorkspaceRoot(root));
                if let Some(analysis) = analysis {
                    self.pending.push_back(analysis);
                }
            }
            EngineCommand::ProjectChange {
                mut refresh,
                mut reanalyze,
                mut uris,
            } => {
                if uris.len() > MAX_PENDING_WATCHED_FILES {
                    refresh = true;
                    reanalyze = true;
                    uris.clear();
                }
                if let Some(EngineCommand::ProjectChange {
                    refresh: pending_refresh,
                    reanalyze: pending_reanalyze,
                    uris: pending_uris,
                }) = self.pending.back_mut()
                {
                    Self::merge_project_change(
                        pending_refresh,
                        pending_reanalyze,
                        pending_uris,
                        refresh,
                        reanalyze,
                        uris,
                    );
                } else {
                    self.pending.push_back(EngineCommand::ProjectChange {
                        refresh,
                        reanalyze,
                        uris,
                    });
                }
            }
        }
    }

    /// Interactive work first, then the neighbourhood, then the sweep. The levels are the
    /// priority, so there is no comparator and no heap.
    fn take(&mut self) -> Option<EngineCommand> {
        if let Some(command) = self.pending.pop_front() {
            return Some(command);
        }
        loop {
            let job = match self.neighborhood.pop_front() {
                Some(job) => job,
                None => self.sweep.pop_front()?,
            };
            let Some(job) = self.claim(job) else {
                // Every URI in the chunk was promoted to a higher level; it is a stale duplicate.
                continue;
            };
            self.indexed_done = self.indexed_done.saturating_add(job.uris.len());
            return Some(EngineCommand::Index(job));
        }
    }

    /// Drop the URIs this chunk no longer owns, then release the rest so a file that changes while
    /// its chunk waits can be offered again.
    fn claim(&mut self, mut job: IndexJob) -> Option<IndexJob> {
        let mut claimed = Vec::with_capacity(job.uris.len());
        for uri in job.uris.drain(..) {
            let owner = self.queued.get(&uri).copied();
            if owner.is_some_and(|entry| entry.priority == job.priority) {
                let entry = self
                    .queued
                    .remove(&uri)
                    .expect("the matching queue owner was just observed");
                let one_copy = workspace_index_uri_bytes(&uri);
                let released = if entry.stale_sweep_copy {
                    // Keep the old sweep string charged until its stale chunk is discarded below.
                    one_copy.saturating_mul(2)
                } else {
                    one_copy.saturating_mul(3)
                };
                self.queued_bytes = self.queued_bytes.saturating_sub(released);
                claimed.push(uri);
            } else if job.priority == IndexPriority::Sweep {
                // Same-priority duplicates are rejected at admission, so a sweep item without a
                // sweep owner is precisely the stale copy left by promotion.
                self.queued_bytes = self
                    .queued_bytes
                    .saturating_sub(workspace_index_uri_bytes(&uri));
            }
        }
        if claimed.is_empty() {
            return None;
        }
        job.uris = claimed;
        Some(job)
    }

    fn indexing_outstanding(&self) -> bool {
        // `queued` is the ownership authority. Priority promotion deliberately leaves a stale sweep
        // copy in its deque; counting raw chunks would keep the progress token open after the promoted
        // URI had finished, even though `take` will discard that stale copy without producing work.
        !self.queued.is_empty()
    }

    /// `(files handed out, files seen)` for this sweep. Reset once nothing is queued, so the next
    /// sweep counts from zero rather than continuing a stale total.
    fn indexing_progress(&mut self) -> (usize, usize) {
        if !self.indexing_outstanding() {
            let done = self.indexed_done;
            let total = self.indexed_total;
            self.indexed_done = 0;
            self.indexed_total = 0;
            return (done, total);
        }
        (self.indexed_done, self.indexed_total)
    }

    /// Work queued against the previous model must never run against its replacement.
    fn replace_index_generation(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.indexed_done = 0;
        self.indexed_total = 0;
        self.neighborhood.clear();
        self.sweep.clear();
        self.queued.clear();
        self.queued_bytes = 0;
        self.index_admission_truncated = false;
        self.generation
    }

    fn push_index_chunk(&mut self, priority: IndexPriority, uris: Vec<String>) {
        let job = IndexJob {
            generation: self.generation,
            priority,
            uris,
        };
        match priority {
            IndexPriority::Neighborhood => self.neighborhood.push_back(job),
            IndexPriority::Sweep => self.sweep.push_back(job),
        }
    }

    fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.neighborhood.is_empty() && self.sweep.is_empty()
    }

    fn compact_project_changes(&mut self) {
        let mut compacted = VecDeque::with_capacity(self.pending.len());
        while let Some(command) = self.pending.pop_front() {
            if let EngineCommand::ProjectChange {
                refresh,
                reanalyze,
                uris,
            } = command
            {
                if let Some(EngineCommand::ProjectChange {
                    refresh: pending_refresh,
                    reanalyze: pending_reanalyze,
                    uris: pending_uris,
                }) = compacted.back_mut()
                {
                    Self::merge_project_change(
                        pending_refresh,
                        pending_reanalyze,
                        pending_uris,
                        refresh,
                        reanalyze,
                        uris,
                    );
                } else {
                    compacted.push_back(EngineCommand::ProjectChange {
                        refresh,
                        reanalyze,
                        uris,
                    });
                }
            } else {
                compacted.push_back(command);
            }
        }
        self.pending = compacted;
    }

    fn merge_project_change(
        pending_refresh: &mut bool,
        pending_reanalyze: &mut bool,
        pending_uris: &mut Vec<String>,
        refresh: bool,
        reanalyze: bool,
        uris: Vec<String>,
    ) {
        *pending_refresh |= refresh;
        *pending_reanalyze |= reanalyze;
        if !*pending_reanalyze || !pending_uris.is_empty() {
            if uris.len() > MAX_PENDING_WATCHED_FILES.saturating_sub(pending_uris.len()) {
                *pending_refresh = true;
                *pending_reanalyze = true;
                pending_uris.clear();
            } else {
                pending_uris.extend(uris);
                pending_uris.sort_unstable();
                pending_uris.dedup();
            }
        }
    }
}

impl CommandReceiver {
    /// Queue work produced by the engine itself, such as the sweep raised after a model loads.
    fn enqueue(&self, command: EngineCommand) {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.disconnected {
            state.enqueue(command);
        }
    }

    fn indexing_progress(&self) -> (usize, usize) {
        self.queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .indexing_progress()
    }

    fn interactive_pending(&self) -> bool {
        !self
            .queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pending
            .is_empty()
    }

    fn indexing_outstanding(&self) -> bool {
        self.queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .indexing_outstanding()
    }

    fn index_generation(&self) -> u64 {
        self.queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
    }

    fn index_admission_truncated(&self) -> bool {
        self.queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .index_admission_truncated
    }

    /// Replace the model generation from inside the engine after a deferred project refresh.
    /// Root replacement does this when it is enqueued; refresh replacement is discovered only
    /// after the provider has run, so it must share the same queue-owned transition here.
    fn replace_index_generation(&self) -> u64 {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let generation = state.replace_index_generation();
        self.queue.ready.notify_one();
        generation
    }

    fn recv(&self, timeout: Option<Duration>) -> CommandReceive {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        loop {
            if let Some(command) = state.pending.pop_front() {
                return CommandReceive::Command(command);
            }
            if state.disconnected {
                return CommandReceive::Disconnected;
            }
            // An overdue project refresh or analysis retry outranks background indexing; checking
            // the deadline here is what stops a nonempty sweep from starving it indefinitely.
            if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                return CommandReceive::Timeout;
            }
            if let Some(command) = state.take() {
                return CommandReceive::Command(command);
            }
            if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return CommandReceive::Timeout;
                }
                let (next, result) = self
                    .queue
                    .ready
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state = next;
                if result.timed_out() && state.is_empty() {
                    return CommandReceive::Timeout;
                }
            } else {
                state = self
                    .queue
                    .ready
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        }
    }
}

pub(crate) struct EngineBackend {
    engine: AnalysisEngine,
    ready: bool,
}

impl EngineBackend {
    pub(crate) fn new(engine: AnalysisEngine, ready: bool) -> Self {
        EngineBackend { engine, ready }
    }

    pub(crate) fn into_engine(self) -> AnalysisEngine {
        self.engine
    }
}

impl AnalysisBackend for EngineBackend {
    fn analysis_ready(&self) -> bool {
        self.ready
    }

    fn accepts_document_set(&self, documents: &[(&str, usize)]) -> bool {
        self.engine.accepts_document_set(documents)
    }

    fn submit(&mut self, job: AnalysisJob) -> Option<AnalysisBatch> {
        debug_assert!(self.ready);
        self.engine.submit(EngineCommand::Analyze(job));
        None
    }

    fn materialize(&mut self, job: MaterializeJob) -> Option<MaterializeResult> {
        self.engine.submit(EngineCommand::Materialize(job));
        None
    }

    fn dump(&mut self, job: DumpJob) -> Option<DumpOutcome> {
        self.engine.submit(EngineCommand::Dump(job));
        None
    }

    fn set_workspace_root(&mut self, root: Option<std::path::PathBuf>) -> Option<ProjectFeedback> {
        self.engine.submit(EngineCommand::SetWorkspaceRoot(root));
        None
    }

    fn watched_globs(&mut self) -> Vec<String> {
        Vec::new()
    }

    fn note_project_change(&mut self) {
        self.engine.submit(EngineCommand::ProjectChange {
            refresh: true,
            reanalyze: false,
            uris: Vec::new(),
        });
    }

    fn note_watched_file_change(&mut self, uri: &str) -> bool {
        self.engine.submit(EngineCommand::ProjectChange {
            refresh: false,
            reanalyze: false,
            uris: vec![uri.to_string()],
        });
        true
    }

    fn note_watched_file_changes(&mut self, uris: &[String]) -> bool {
        self.engine.submit(EngineCommand::ProjectChange {
            refresh: false,
            reanalyze: false,
            uris: uris.to_vec(),
        });
        true
    }

    fn project_refresh_due_in(&self) -> Option<std::time::Duration> {
        None
    }

    fn refresh_project(&mut self) -> Option<ProjectFeedback> {
        None
    }

    fn set_ready(&mut self, ready: bool) {
        self.ready = ready;
    }
}

fn run<A: Analysis>(
    mut analyze: A,
    commands: CommandReceiver,
    events: SyncSender<Incoming>,
    admission: Arc<RwLock<DocumentAdmission>>,
) {
    update_admission(&admission, analyze.document_admission());
    let mut last_ready = analyze.analysis_ready();
    let mut submitted_sweep_generation = None;
    let _ = events.send(Incoming::Engine(EngineEvent::ReadyState(last_ready)));
    // Reconfiguration and analysis must remain ordered on this thread.
    loop {
        let command = match commands.recv(analyze.project_refresh_due_in()) {
            CommandReceive::Command(command) => Some(command),
            CommandReceive::Timeout => None,
            CommandReceive::Disconnected => break,
        };
        let working = match &command {
            None => Some("Refreshing project".to_string()),
            Some(EngineCommand::SetWorkspaceRoot(_)) => Some("Loading project".to_string()),
            Some(EngineCommand::Analyze(job)) => {
                Some(format!("Analyzing {} files", job.documents.len()))
            }
            Some(EngineCommand::Index(_)) => {
                let (done, total) = commands.indexing_progress();
                // Report the operation, not the chunk: "Indexing 32 files" never moved on a large
                // workspace, so it was impossible to tell a running sweep from a finished one.
                Some(format!("Indexing workspace: {done} of {total} files"))
            }
            _ => None,
        };
        if let Some(message) = working {
            if send_status(&events, ServerStatus::Working(message)).is_err() {
                break;
            }
        }
        match command {
            None => {
                let feedback = analyze_refresh(&mut analyze);
                if feedback.reanalyze {
                    let generation = commands.replace_index_generation();
                    submitted_sweep_generation = None;
                    if events
                        .send(Incoming::Engine(EngineEvent::IndexReset(generation)))
                        .is_err()
                    {
                        break;
                    }
                }
                update_admission(&admission, analyze.document_admission());
                if emit_project(&events, &mut analyze, feedback, &mut last_ready).is_err() {
                    break;
                }
            }
            Some(EngineCommand::SetWorkspaceRoot(root)) => {
                let feedback = analyze.set_workspace_root(root);
                let generation = commands.index_generation();
                submitted_sweep_generation = None;
                if events
                    .send(Incoming::Engine(EngineEvent::IndexReset(generation)))
                    .is_err()
                {
                    break;
                }
                update_admission(&admission, analyze.document_admission());
                let globs = analyze.watched_globs();
                if events
                    .send(Incoming::Engine(EngineEvent::WatchedGlobs(globs)))
                    .is_err()
                {
                    break;
                }
                if emit_project(&events, &mut analyze, feedback, &mut last_ready).is_err() {
                    break;
                }
            }
            Some(EngineCommand::Analyze(job)) => {
                let docs: Vec<(&str, &str)> = job
                    .documents
                    .iter()
                    .map(|(uri, text, _)| (uri.as_str(), text.as_str()))
                    .collect();
                let open: Vec<&str> = job.open_uris.iter().map(String::as_str).collect();
                let (analyses, support_documents) = analyze.analyze_open_documents(&docs, &open);
                let analyzed = job
                    .documents
                    .iter()
                    .map(|(uri, _, version)| (uri.clone(), *version))
                    .collect();
                let batch = AnalysisBatch {
                    analyzed,
                    analyses,
                    support_documents,
                    pending: analyze.analysis_pending(),
                };
                if events
                    .send(Incoming::Engine(EngineEvent::AnalysisComplete(batch)))
                    .is_err()
                {
                    break;
                }
                if send_status(&events, ServerStatus::Ready).is_err() {
                    break;
                }
                // Raised only after an interactive analysis has been served, and only while no
                // further interactive work is waiting. Enumerating a large workspace ahead of the
                // first open document delayed its diagnostics past two minutes on a 64k-file tree.
                if !commands.interactive_pending() {
                    let neighborhood = analyze.neighborhood_index_candidates(&open);
                    if !neighborhood.is_empty() {
                        commands.enqueue(EngineCommand::Index(IndexJob {
                            generation: 0,
                            priority: IndexPriority::Neighborhood,
                            uris: neighborhood,
                        }));
                    }
                    let generation = commands.index_generation();
                    if submitted_sweep_generation != Some(generation) {
                        submit_workspace_sweep(&mut analyze, &commands);
                        if (analyze.workspace_index_incomplete()
                            || commands.index_admission_truncated())
                            && events
                                .send(Incoming::Engine(EngineEvent::Project(ProjectFeedback {
                                    logs: vec![
                                        "krusty: workspace diagnostic inventory reached its \
                                         traversal or queue limit; background results are incomplete"
                                            .to_string(),
                                    ],
                                    ..ProjectFeedback::default()
                                })))
                                .is_err()
                        {
                            break;
                        }
                        submitted_sweep_generation = Some(generation);
                    }
                }
            }
            Some(EngineCommand::Materialize(job)) => {
                let definition = analyze.materialize_library_definition(&job.reference);
                if events
                    .send(Incoming::Engine(EngineEvent::Materialized(
                        MaterializeResult {
                            token: job.token,
                            definition,
                        },
                    )))
                    .is_err()
                {
                    break;
                }
            }
            Some(EngineCommand::Dump(job)) => {
                let dump = analyze.dump(&job.uri);
                if events
                    .send(Incoming::Engine(EngineEvent::Dumped(DumpOutcome {
                        token: job.token,
                        dump,
                    })))
                    .is_err()
                {
                    break;
                }
            }
            Some(EngineCommand::Index(job)) => {
                let uris: Vec<&str> = job.uris.iter().map(String::as_str).collect();
                let outcome = analyze.index_workspace_files(&uris);
                let outstanding = commands.indexing_outstanding();
                if events
                    .send(Incoming::Engine(EngineEvent::IndexProgress(IndexBatch {
                        generation: job.generation,
                        attempted: job.uris,
                        files: outcome.files,
                        conclusive: outcome.conclusive,
                    })))
                    .is_err()
                {
                    break;
                }
                // Progress spans the whole sweep: reporting Ready per chunk would open and close a
                // token thousands of times over a large workspace.
                // Ready only when the sweep is done, so the progress token spans the whole
                // operation and each chunk updates its message instead of reopening it.
                if !outstanding && send_status(&events, ServerStatus::Ready).is_err() {
                    break;
                }
            }
            Some(EngineCommand::ProjectChange {
                refresh,
                mut reanalyze,
                uris,
            }) => {
                if refresh {
                    analyze.note_project_change();
                }
                let mut changed_sources = Vec::new();
                for uri in &uris {
                    if analyze.note_watched_file_change(uri) {
                        reanalyze = true;
                        changed_sources.push(uri.clone());
                    }
                }
                if !changed_sources.is_empty() {
                    // Direct changes do not need a reverse-dependency graph: the originating file
                    // can always be refreshed (or tombstoned) immediately. Dependents remain a
                    // separate incremental-indexing slice.
                    commands.enqueue(EngineCommand::Index(IndexJob {
                        generation: 0,
                        priority: IndexPriority::Neighborhood,
                        uris: changed_sources,
                    }));
                }
                if reanalyze
                    && events
                        .send(Incoming::Engine(EngineEvent::ReanalyzeRequested))
                        .is_err()
                {
                    break;
                }
            }
        }
    }
}

/// Raise one sweep over everything the current project-model generation knows about. Direct watched
/// changes are queued separately; repeating this inventory after every interactive analysis would
/// put an unqueued full tree walk back on the latency-sensitive engine thread.
fn submit_workspace_sweep<A: Analysis>(analyze: &mut A, commands: &CommandReceiver) {
    let uris = analyze.workspace_index_candidates();
    if uris.is_empty() {
        return;
    }
    commands.enqueue(EngineCommand::Index(IndexJob {
        generation: 0,
        priority: IndexPriority::Sweep,
        uris,
    }));
}

fn update_admission(admission: &RwLock<DocumentAdmission>, document_admission: DocumentAdmission) {
    *admission
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = document_admission;
}

fn analyze_refresh<A: Analysis>(analyze: &mut A) -> ProjectFeedback {
    analyze.refresh_project()
}

fn emit_project<A: Analysis>(
    events: &SyncSender<Incoming>,
    analyze: &mut A,
    feedback: ProjectFeedback,
    last_ready: &mut bool,
) -> Result<(), ()> {
    events
        .send(Incoming::Engine(EngineEvent::Project(feedback)))
        .map_err(|_| ())?;
    let ready = analyze.analysis_ready();
    if ready != *last_ready {
        *last_ready = ready;
        events
            .send(Incoming::Engine(EngineEvent::ReadyState(ready)))
            .map_err(|_| ())?;
    }
    send_status(events, ServerStatus::Ready)
}

fn send_status(events: &SyncSender<Incoming>, status: ServerStatus) -> Result<(), ()> {
    events
        .send(Incoming::Engine(EngineEvent::Status(status)))
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::super::super::IndexOutcome;
    use super::*;

    #[test]
    fn command_queue_bounds_project_change_bursts() {
        let mut state = CommandState::default();
        for index in 0..(MAX_PENDING_WATCHED_FILES + 1) {
            state.enqueue(EngineCommand::ProjectChange {
                refresh: false,
                reanalyze: false,
                uris: vec![format!("file:///{index}.kt")],
            });
        }

        assert_eq!(state.pending.len(), 1);
        let EngineCommand::ProjectChange {
            refresh,
            reanalyze,
            uris,
        } = state.pending.pop_front().unwrap()
        else {
            panic!("expected a coalesced project change");
        };
        assert!(refresh);
        assert!(reanalyze);
        assert!(uris.is_empty());
    }

    #[test]
    fn workspace_reconfiguration_stays_before_pending_analysis() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///old.kt".into(), String::new(), 1)],
            open_uris: Vec::new(),
        }));
        state.enqueue(EngineCommand::SetWorkspaceRoot(Some("/workspace".into())));

        assert!(matches!(
            state.pending.pop_front(),
            Some(EngineCommand::SetWorkspaceRoot(_))
        ));
        assert!(matches!(
            state.pending.pop_front(),
            Some(EngineCommand::Analyze(_))
        ));
    }

    #[test]
    fn command_queue_replaces_obsolete_analysis_without_growing() {
        let mut state = CommandState::default();
        for index in 0..100 {
            state.enqueue(EngineCommand::ProjectChange {
                refresh: false,
                reanalyze: false,
                uris: vec![format!("file:///{index}.kt")],
            });
            state.enqueue(EngineCommand::Analyze(AnalysisJob {
                documents: vec![(format!("file:///{index}.kt"), String::new(), 1)],
                open_uris: Vec::new(),
            }));
        }

        assert_eq!(state.pending.len(), 2);
        assert!(matches!(
            state.pending.pop_front(),
            Some(EngineCommand::ProjectChange { .. })
        ));
        assert!(matches!(
            state.pending.pop_front(),
            Some(EngineCommand::Analyze(_))
        ));
    }

    #[test]
    fn a_queued_dump_is_not_reordered_behind_a_later_analysis() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Dump(DumpJob {
            token: 0,
            uri: "file:///a.kt".into(),
        }));
        state.enqueue(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///a.kt".into(), "fun a(){}".into(), 2)],
            open_uris: Vec::new(),
        }));

        // The dump runs first, so it replays the payload the previous pass retained rather than the
        // edit still queued behind it. Nothing in the queue makes a dump current; only the source
        // hash in the rendered header says which text it saw.
        assert!(matches!(
            state.pending.pop_front(),
            Some(EngineCommand::Dump(_))
        ));
        assert!(matches!(
            state.pending.pop_front(),
            Some(EngineCommand::Analyze(_))
        ));
    }

    #[test]
    fn join_terminates_the_thread_without_a_prior_shutdown_command() {
        use std::sync::mpsc::sync_channel;

        struct Mock;
        impl Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
        }

        let (tx, _rx) = sync_channel(4);
        let engine = AnalysisEngine::spawn(Mock, tx);
        engine.join();
    }

    #[test]
    fn job_and_batch_round_trip_fields() {
        let job = AnalysisJob {
            documents: vec![("file:///a.kt".into(), "fun a(){}".into(), 3)],
            open_uris: vec!["file:///a.kt".into()],
        };
        assert_eq!(job.documents[0].2, 3);

        let batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 3)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        };
        assert_eq!(batch.analyzed[0].1, 3);
        assert_eq!(batch.analyses.len(), 1);
    }

    #[test]
    fn analyze_command_produces_completion_event() {
        use std::sync::mpsc::sync_channel;

        struct Mock;
        impl Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
        }

        let (tx, rx) = sync_channel(4);
        let engine = AnalysisEngine::spawn(Mock, tx);
        engine.submit(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///a.kt".into(), "fun a(){}".into(), 2)],
            open_uris: vec!["file:///a.kt".into()],
        }));
        let mut found = false;
        for _ in 0..4 {
            match rx.recv().unwrap() {
                Incoming::Engine(EngineEvent::AnalysisComplete(batch)) => {
                    assert_eq!(batch.analyzed, vec![("file:///a.kt".to_string(), 2)]);
                    assert_eq!(batch.analyses.len(), 1);
                    found = true;
                    break;
                }
                Incoming::Engine(EngineEvent::ReadyState(_)) => {}
                Incoming::Engine(EngineEvent::Status(_)) => {}
                _ => panic!("unexpected event"),
            }
        }
        assert!(found, "expected AnalysisComplete event");
        engine.join();
    }

    #[test]
    fn materialize_command_produces_correlated_event() {
        use std::sync::mpsc::sync_channel;

        struct Mock;
        impl Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }

            fn materialize_library_definition(
                &mut self,
                _reference: &LibraryRef,
            ) -> Option<MaterializedDefinition> {
                Some(MaterializedDefinition {
                    path: "/cache/Type.kt".into(),
                    text: "class Type".into(),
                    lo: 6,
                    hi: 10,
                })
            }
        }

        let (tx, rx) = sync_channel(4);
        let engine = AnalysisEngine::spawn(Mock, tx);
        engine.submit(EngineCommand::Materialize(MaterializeJob {
            token: 7,
            reference: LibraryRef {
                fqn: "sample/Type".into(),
                member_name: String::new(),
                member_desc: String::new(),
            },
        }));
        let result = (0..2).find_map(|_| match rx.recv().unwrap() {
            Incoming::Engine(EngineEvent::Materialized(result)) => Some(result),
            Incoming::Engine(EngineEvent::ReadyState(_)) => None,
            _ => panic!("unexpected event"),
        });
        let result = result.expect("materialize event");
        assert_eq!(result.token, 7);
        assert_eq!(result.definition.unwrap().lo, 6);
        engine.join();
    }

    #[test]
    fn analyze_after_reconfigure_reflects_the_reconfigured_worker() {
        use std::sync::mpsc::sync_channel;

        struct ReconfigureMock {
            reconfigured: bool,
        }
        impl Analysis for ReconfigureMock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
            fn set_workspace_root(
                &mut self,
                _root: Option<std::path::PathBuf>,
            ) -> crate::server::implementation::ProjectFeedback {
                self.reconfigured = true;
                crate::server::implementation::ProjectFeedback::default()
            }
            fn analyze_open_documents(
                &mut self,
                documents: &[(&str, &str)],
                _open_uris: &[&str],
            ) -> (Vec<DocumentAnalysis>, Vec<(String, String)>) {
                let state = if self.reconfigured {
                    "reconfigured"
                } else {
                    "stale"
                };
                (
                    documents
                        .iter()
                        .map(|_| DocumentAnalysis::empty())
                        .collect(),
                    vec![("state".to_string(), state.to_string())],
                )
            }
        }

        let (tx, rx) = sync_channel(8);
        let engine = AnalysisEngine::spawn(
            ReconfigureMock {
                reconfigured: false,
            },
            tx,
        );
        engine.submit(EngineCommand::SetWorkspaceRoot(None));
        engine.submit(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///a.kt".into(), "fun a(){}".into(), 1)],
            open_uris: vec!["file:///a.kt".into()],
        }));

        let mut support = None;
        loop {
            match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(Incoming::Engine(EngineEvent::AnalysisComplete(batch))) => {
                    support = Some(batch.support_documents);
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert_eq!(
            support,
            Some(vec![("state".to_string(), "reconfigured".to_string())]),
            "analysis published after a reconfigure must reflect the reconfigured worker"
        );
        engine.join();
    }

    #[test]
    fn set_workspace_root_emits_globs_ready_and_project() {
        use std::sync::mpsc::sync_channel;
        struct Mock {
            ready: bool,
        }
        impl crate::server::implementation::Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, s: &[&str]) -> Vec<DocumentAnalysis> {
                s.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
            fn analysis_ready(&self) -> bool {
                self.ready
            }
            fn set_workspace_root(
                &mut self,
                _root: Option<std::path::PathBuf>,
            ) -> crate::server::implementation::ProjectFeedback {
                self.ready = true;
                crate::server::implementation::ProjectFeedback {
                    reanalyze: true,
                    message: None,
                    logs: vec!["loaded".into()],
                }
            }
            fn watched_globs(&mut self) -> Vec<String> {
                vec!["**/*.kt".into()]
            }
        }
        let (tx, rx) = sync_channel(8);
        let engine = AnalysisEngine::spawn(Mock { ready: false }, tx);
        engine.submit(EngineCommand::SetWorkspaceRoot(None));

        let mut saw_globs = false;
        let mut saw_ready = false;
        let mut saw_project = false;
        for _ in 0..8 {
            match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                Ok(Incoming::Engine(EngineEvent::WatchedGlobs(g))) => {
                    assert_eq!(g, vec!["**/*.kt".to_string()]);
                    saw_globs = true;
                }
                Ok(Incoming::Engine(EngineEvent::ReadyState(true))) => saw_ready = true,
                Ok(Incoming::Engine(EngineEvent::Project(f))) => {
                    assert!(f.reanalyze);
                    saw_project = true;
                }
                _ => {}
            }
            if saw_globs && saw_ready && saw_project {
                break;
            }
        }
        assert!(saw_globs && saw_ready && saw_project);
        engine.join();
    }

    #[test]
    fn engine_backend_submit_is_async() {
        use std::sync::mpsc::sync_channel;
        struct Mock;
        impl crate::server::implementation::Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, s: &[&str]) -> Vec<DocumentAnalysis> {
                s.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
        }
        let (tx, rx) = sync_channel(4);
        let mut backend = EngineBackend::new(AnalysisEngine::spawn(Mock, tx), false);
        backend.set_ready(true);
        let now = backend.submit(AnalysisJob {
            documents: vec![("file:///a.kt".into(), "x".into(), 1)],
            open_uris: vec!["file:///a.kt".into()],
        });
        assert!(now.is_none(), "engine backend is asynchronous");
        let mut found = false;
        for _ in 0..4 {
            match rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap() {
                Incoming::Engine(EngineEvent::AnalysisComplete(_)) => {
                    found = true;
                    break;
                }
                Incoming::Engine(EngineEvent::ReadyState(_)) => {}
                Incoming::Engine(EngineEvent::Status(_)) => {}
                _ => panic!("unexpected event"),
            }
        }
        assert!(found, "expected AnalysisComplete event");
        backend.into_engine().join();
    }

    #[test]
    fn engine_backend_reads_the_latest_document_admission_snapshot() {
        use crate::project::{Module, ModuleId, ProjectModel, ProviderKind, SourceRoot};
        use crate::server::implementation::DocumentAdmission;
        use std::sync::mpsc::sync_channel;

        struct Mock {
            admission: DocumentAdmission,
        }
        impl crate::server::implementation::Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }

            fn document_admission(&self) -> DocumentAdmission {
                self.admission.clone()
            }

            fn set_workspace_root(
                &mut self,
                _root: Option<std::path::PathBuf>,
            ) -> crate::server::implementation::ProjectFeedback {
                let mut first = Module::new(ModuleId::new(":first", "main"), "/workspace/first");
                first.source_roots = vec![SourceRoot::source("/workspace/first/src")];
                let mut second = Module::new(ModuleId::new(":second", "main"), "/workspace/second");
                second.source_roots = vec![SourceRoot::source("/workspace/second/src")];
                let model = ProjectModel::new("/workspace", ProviderKind::Gradle)
                    .with_modules(vec![first, second]);
                self.admission = DocumentAdmission::for_model(&model);
                crate::server::implementation::ProjectFeedback::default()
            }
        }

        let (tx, rx) = sync_channel(8);
        let mut backend = EngineBackend::new(
            AnalysisEngine::spawn(
                Mock {
                    admission: DocumentAdmission::default(),
                },
                tx,
            ),
            true,
        );
        backend.set_workspace_root(None);
        loop {
            if matches!(
                rx.recv_timeout(std::time::Duration::from_secs(1)),
                Ok(Incoming::Engine(EngineEvent::Project(_)))
            ) {
                break;
            }
        }
        let large = crate::worker::MAX_SOURCE_SET_BYTES / 2 + 1;
        assert!(backend.accepts_document_set(&[
            ("file:///workspace/first/src/First.kt", large),
            ("file:///workspace/second/src/Second.kt", large),
        ]));
        backend.into_engine().join();
    }

    #[test]
    fn watched_file_change_batch_folds_into_at_most_one_reanalyze() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc::sync_channel;
        use std::sync::Arc;

        struct Mock {
            per_uri_calls: Arc<AtomicUsize>,
        }
        impl crate::server::implementation::Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, s: &[&str]) -> Vec<DocumentAnalysis> {
                s.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
            fn note_watched_file_change(&mut self, uri: &str) -> bool {
                self.per_uri_calls.fetch_add(1, Ordering::SeqCst);
                uri.ends_with(".kt")
            }
        }

        let per_uri_calls = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = sync_channel(64);
        let mut backend = EngineBackend::new(
            AnalysisEngine::spawn(
                Mock {
                    per_uri_calls: per_uri_calls.clone(),
                },
                tx,
            ),
            false,
        );

        let uris: Vec<String> = (0..20).map(|i| format!("file:///p/File{i}.kt")).collect();
        let now = backend.note_watched_file_changes(&uris);
        assert!(
            now,
            "the async backend invalidates cached analysis immediately"
        );

        let mut reanalyze = 0;
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(300)) {
                Ok(Incoming::Engine(EngineEvent::ReanalyzeRequested)) => reanalyze += 1,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert_eq!(
            per_uri_calls.load(Ordering::SeqCst),
            20,
            "the engine folds every change in the batch"
        );
        assert_eq!(
            reanalyze, 1,
            "at most one ReanalyzeRequested is emitted per notification"
        );
        backend.into_engine().join();
    }

    #[test]
    fn run_loop_reports_project_and_analysis_work() {
        use std::sync::mpsc::sync_channel;
        use std::time::Duration;

        struct Mock;
        impl Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, s: &[&str]) -> Vec<DocumentAnalysis> {
                s.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
            fn set_workspace_root(&mut self, _root: Option<std::path::PathBuf>) -> ProjectFeedback {
                ProjectFeedback::default()
            }
        }

        let (tx, rx) = sync_channel(16);
        let engine = AnalysisEngine::spawn(Mock, tx);
        engine.submit(EngineCommand::SetWorkspaceRoot(None));
        engine.submit(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///a.kt".into(), "fun a(){}".into(), 1)],
            open_uris: vec!["file:///a.kt".into()],
        }));

        let mut statuses = Vec::new();
        while statuses.len() < 4 {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Incoming::Engine(EngineEvent::Status(status))) => statuses.push(status),
                Ok(_) => {}
                Err(_) => break,
            }
        }

        assert_eq!(
            statuses,
            vec![
                ServerStatus::Working("Loading project".to_string()),
                ServerStatus::Ready,
                ServerStatus::Working("Analyzing 1 files".to_string()),
                ServerStatus::Ready,
            ]
        );

        engine.join();
    }
    #[test]
    fn an_index_command_produces_a_progress_event() {
        use std::sync::mpsc::sync_channel;

        struct Mock;
        impl Analysis for Mock {
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }

            fn index_workspace_files(&mut self, uris: &[&str]) -> IndexOutcome {
                let files = uris
                    .iter()
                    .map(|uri| IndexedFile {
                        uri: (*uri).to_string(),
                        diagnostics: Vec::new(),
                        text_hash: 7,
                        text: String::new(),
                    })
                    .collect();
                IndexOutcome {
                    files,
                    conclusive: true,
                }
            }
        }

        let (tx, rx) = sync_channel(8);
        let engine = AnalysisEngine::spawn(Mock, tx);
        engine.submit(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/A.kt".into(), "file:///w/B.kt".into()],
        }));

        let batch = (0..8).find_map(|_| match rx.recv().unwrap() {
            Incoming::Engine(EngineEvent::IndexProgress(batch)) => Some(batch),
            _ => None,
        });
        let batch = batch.expect("index progress event");
        assert_eq!(
            batch
                .files
                .iter()
                .map(|f| f.uri.as_str())
                .collect::<Vec<_>>(),
            vec!["file:///w/A.kt", "file:///w/B.kt"]
        );
        assert_eq!(batch.files[0].text_hash, 7);
        assert!(batch.conclusive);
        assert_eq!(batch.attempted.len(), 2);
        engine.join();
    }

    #[test]
    fn a_watched_source_change_is_reindexed_after_the_initial_sweep() {
        use std::sync::mpsc::sync_channel;

        struct Mock;
        impl Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome {
                    files: Vec::new(),
                    conclusive: true,
                }
            }

            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }

            fn note_watched_file_change(&mut self, uri: &str) -> bool {
                uri.ends_with(".kt")
            }
        }

        let (events, incoming) = sync_channel(16);
        let engine = AnalysisEngine::spawn(Mock, events);
        engine.submit(EngineCommand::ProjectChange {
            refresh: false,
            reanalyze: false,
            uris: vec!["file:///w/Changed.kt".to_string()],
        });

        loop {
            match incoming.recv_timeout(Duration::from_secs(1)) {
                Ok(Incoming::Engine(EngineEvent::IndexProgress(batch))) => {
                    assert_eq!(batch.attempted, ["file:///w/Changed.kt"]);
                    assert!(batch.conclusive);
                    break;
                }
                Ok(_) => {}
                Err(error) => panic!("changed source was not indexed: {error}"),
            }
        }
        engine.join();
    }

    #[test]
    fn interactive_commands_are_served_before_queued_index_chunks() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
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
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/Far.kt".into()],
        }));
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
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
    fn index_chunks_are_grouped_by_priority() {
        let mut state = CommandState::default();
        for index in 0..3 {
            state.enqueue(EngineCommand::Index(IndexJob {
                generation: 0,
                priority: IndexPriority::Sweep,
                uris: vec![format!("file:///w/S{index}.kt")],
            }));
        }
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Neighborhood,
            uris: vec!["file:///w/N.kt".into()],
        }));

        let mut sweep = 0;
        let mut neighborhood = 0;
        while let Some(EngineCommand::Index(job)) = state.take() {
            match job.priority {
                IndexPriority::Sweep => sweep += 1,
                IndexPriority::Neighborhood => neighborhood += 1,
            }
        }
        assert_eq!(sweep, 3);
        assert_eq!(neighborhood, 1);
    }
    #[test]
    fn a_large_index_job_is_split_into_bounded_chunks() {
        let mut state = CommandState::default();
        let uris: Vec<String> = (0..(MAX_INDEX_CHUNK_FILES * 2 + 1))
            .map(|index| format!("file:///w/F{index}.kt"))
            .collect();
        let total = uris.len();
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris,
        }));

        let mut chunks = 0;
        let mut seen = 0;
        while let Some(EngineCommand::Index(job)) = state.take() {
            assert!(
                job.uris.len() <= MAX_INDEX_CHUNK_FILES,
                "no chunk may exceed the bound that caps interactive latency"
            );
            chunks += 1;
            seen += job.uris.len();
        }
        assert_eq!(chunks, 3);
        assert_eq!(seen, total);
    }

    #[test]
    fn a_file_already_queued_is_not_queued_again() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/A.kt".into(), "file:///w/B.kt".into()],
        }));
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/B.kt".into(), "file:///w/C.kt".into()],
        }));

        let mut queued = Vec::new();
        while let Some(EngineCommand::Index(job)) = state.take() {
            queued.extend(job.uris);
        }
        queued.sort();
        assert_eq!(
            queued,
            vec!["file:///w/A.kt", "file:///w/B.kt", "file:///w/C.kt"]
        );
    }

    #[test]
    fn taking_a_chunk_releases_its_files_for_requeueing() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/A.kt".into()],
        }));
        assert!(matches!(state.take(), Some(EngineCommand::Index(_))));

        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
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
            generation: 0,
            priority: IndexPriority::Sweep,
            uris,
        }));

        let mut queued = 0;
        while let Some(EngineCommand::Index(job)) = state.take() {
            queued += job.uris.len();
        }
        assert_eq!(queued, MAX_QUEUED_INDEX_FILES);
    }
    #[test]
    fn index_chunks_report_progress_as_working_status() {
        use std::sync::mpsc::sync_channel;
        use std::time::Duration;

        struct Mock;
        impl Analysis for Mock {
            fn analyze(&mut self, s: &[&str]) -> Vec<DocumentAnalysis> {
                s.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
            fn index_workspace_files(&mut self, uris: &[&str]) -> IndexOutcome {
                let files = uris
                    .iter()
                    .map(|uri| IndexedFile {
                        uri: (*uri).to_string(),
                        diagnostics: Vec::new(),
                        text_hash: 0,
                        text: String::new(),
                    })
                    .collect();
                IndexOutcome {
                    files,
                    conclusive: true,
                }
            }
        }

        let (tx, rx) = sync_channel(16);
        let engine = AnalysisEngine::spawn(Mock, tx);
        engine.submit(EngineCommand::Index(IndexJob {
            generation: 0,
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
                ServerStatus::Working("Indexing workspace: 2 of 2 files".to_string()),
                ServerStatus::Ready,
            ]
        );
        engine.join();
    }

    #[test]
    fn multiple_index_chunks_report_monotonic_operation_progress() {
        use std::sync::mpsc::sync_channel;
        use std::time::Duration;

        struct Mock;
        impl Analysis for Mock {
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
            fn index_workspace_files(&mut self, uris: &[&str]) -> IndexOutcome {
                IndexOutcome {
                    files: uris
                        .iter()
                        .map(|uri| IndexedFile {
                            uri: (*uri).to_string(),
                            diagnostics: Vec::new(),
                            text_hash: 0,
                            text: String::new(),
                        })
                        .collect(),
                    conclusive: true,
                }
            }
        }

        let file_count = MAX_INDEX_CHUNK_FILES + 1;
        let (tx, rx) = sync_channel(16);
        let engine = AnalysisEngine::spawn(Mock, tx);
        engine.submit(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: (0..file_count)
                .map(|index| format!("file:///w/F{index}.kt"))
                .collect(),
        }));

        let mut statuses = Vec::new();
        while statuses.len() < 3 {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Incoming::Engine(EngineEvent::Status(status))) => statuses.push(status),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert_eq!(
            statuses,
            vec![
                ServerStatus::Working(format!(
                    "Indexing workspace: {MAX_INDEX_CHUNK_FILES} of {file_count} files"
                )),
                ServerStatus::Working(format!(
                    "Indexing workspace: {file_count} of {file_count} files"
                )),
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
            generation: 0,
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
    #[test]
    fn an_expired_refresh_deadline_wins_over_queued_index_work() {
        let (sender, receiver) = command_queue();
        sender.send(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/Swept.kt".into()],
        }));

        assert!(
            matches!(receiver.recv(Some(Duration::ZERO)), CommandReceive::Timeout),
            "an overdue project refresh must not starve behind background indexing"
        );
    }

    #[test]
    fn a_sweep_file_is_promoted_when_it_becomes_neighborhood_work() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/A.kt".into()],
        }));
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Neighborhood,
            uris: vec!["file:///w/A.kt".into()],
        }));

        let Some(EngineCommand::Index(first)) = state.take() else {
            panic!("expected the promoted chunk");
        };
        assert_eq!(first.priority, IndexPriority::Neighborhood);
        assert_eq!(first.uris, vec!["file:///w/A.kt".to_string()]);
        assert!(
            !state.indexing_outstanding(),
            "a stale sweep copy must not keep the operation marked as active"
        );
        assert_eq!(
            state.indexing_progress(),
            (1, 1),
            "promotion changes priority, not the amount of admitted work"
        );
        assert!(
            state.take().is_none(),
            "the superseded sweep entry must not be indexed a second time"
        );
    }

    #[test]
    fn a_sweep_file_can_be_promoted_at_the_admission_ceiling() {
        let uri = "file:///w/Promoted.kt".to_string();
        let mut state = CommandState::default();
        state.queued.insert(
            uri.clone(),
            QueuedIndexEntry {
                priority: IndexPriority::Sweep,
                stale_sweep_copy: false,
            },
        );
        state.queued_bytes = MAX_QUEUED_INDEX_BYTES;
        state.sweep.push_back(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec![uri.clone()],
        });

        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Neighborhood,
            uris: vec![uri.clone()],
        }));

        let Some(EngineCommand::Index(job)) = state.take() else {
            panic!("promotion must not be rejected as new admission");
        };
        assert_eq!(job.priority, IndexPriority::Neighborhood);
        assert_eq!(job.uris, [uri]);
    }

    #[test]
    fn a_promoted_sweep_copy_stays_charged_until_it_is_discarded() {
        let uri = "file:///w/Promoted.kt".to_string();
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec![uri.clone()],
        }));
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Neighborhood,
            uris: vec![uri],
        }));

        let Some(EngineCommand::Index(job)) = state.take() else {
            panic!("promoted work must run at neighbourhood priority");
        };
        assert_eq!(job.priority, IndexPriority::Neighborhood);
        assert!(
            state.queued_bytes > 0,
            "the superseded sweep string remains retained in its old chunk"
        );
        assert!(state.take().is_none(), "the stale sweep copy must not run");
        assert_eq!(
            state.queued_bytes, 0,
            "discarding the stale chunk must release its final URI reservation"
        );
    }

    #[test]
    fn replacing_the_workspace_root_discards_queued_index_work() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec!["file:///old/A.kt".into()],
        }));
        state.enqueue(EngineCommand::SetWorkspaceRoot(Some("/new".into())));

        assert!(matches!(
            state.take(),
            Some(EngineCommand::SetWorkspaceRoot(_))
        ));
        assert!(
            state.take().is_none(),
            "work queued against the previous model must not run against its replacement"
        );
        assert_eq!(
            state.generation, 1,
            "the model generation moves with the root"
        );
    }

    #[test]
    fn an_index_chunk_carries_the_generation_it_was_queued_under() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::SetWorkspaceRoot(None));
        let _ = state.take();
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec!["file:///w/A.kt".into()],
        }));

        let Some(EngineCommand::Index(job)) = state.take() else {
            panic!("expected an index chunk");
        };
        assert_eq!(
            job.generation, 1,
            "the queue stamps the current generation so late results can be rejected"
        );
    }

    #[test]
    fn the_index_queue_stops_growing_at_its_byte_bound() {
        let mut state = CommandState::default();
        let long = "x".repeat(4096);
        let uris: Vec<String> = (0..4096)
            .map(|index| format!("file:///w/{long}{index}.kt"))
            .collect();
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris,
        }));

        assert!(
            state.queued_bytes <= MAX_QUEUED_INDEX_BYTES,
            "the queue bounds retained URI bytes, not just the file count"
        );
        assert!(state.queued_bytes > 0, "some work is still admitted");
        assert!(
            state.index_admission_truncated,
            "dropped inventory must be surfaced as incomplete rather than silently omitted"
        );
    }

    #[test]
    fn a_chunk_reports_whether_indexing_is_still_outstanding() {
        let mut state = CommandState::default();
        let uris: Vec<String> = (0..(MAX_INDEX_CHUNK_FILES + 1))
            .map(|index| format!("file:///w/F{index}.kt"))
            .collect();
        state.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris,
        }));

        let _ = state.take();
        assert!(
            state.indexing_outstanding(),
            "progress stays open while chunks remain"
        );
        let _ = state.take();
        assert!(
            !state.indexing_outstanding(),
            "progress closes once the last chunk is handed out"
        );
    }

    #[test]
    fn a_model_refresh_replaces_the_queue_generation_and_discards_old_work() {
        let (_sender, receiver) = command_queue();
        receiver.enqueue(EngineCommand::Index(IndexJob {
            generation: 0,
            priority: IndexPriority::Sweep,
            uris: vec!["file:///old/A.kt".into()],
        }));

        assert_eq!(receiver.replace_index_generation(), 1);
        assert!(
            matches!(receiver.recv(Some(Duration::ZERO)), CommandReceive::Timeout),
            "a refreshed model must not execute work discovered under its predecessor"
        );
    }

    #[test]
    fn a_workspace_sweep_is_enumerated_once_per_model_generation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc::sync_channel;
        use std::sync::Arc;

        struct Mock {
            inventories: Arc<AtomicUsize>,
        }
        impl Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome {
                    files: Vec::new(),
                    conclusive: true,
                }
            }

            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }

            fn workspace_index_candidates(&mut self) -> Vec<String> {
                self.inventories.fetch_add(1, Ordering::SeqCst);
                Vec::new()
            }
        }

        let inventories = Arc::new(AtomicUsize::new(0));
        let (events, incoming) = sync_channel(16);
        let engine = AnalysisEngine::spawn(
            Mock {
                inventories: inventories.clone(),
            },
            events,
        );
        for version in [1, 2] {
            engine.submit(EngineCommand::Analyze(AnalysisJob {
                documents: vec![("file:///w/Open.kt".into(), "fun open() {}".into(), version)],
                open_uris: vec!["file:///w/Open.kt".into()],
            }));
            loop {
                match incoming.recv_timeout(Duration::from_secs(1)) {
                    Ok(Incoming::Engine(EngineEvent::AnalysisComplete(_))) => break,
                    Ok(_) => {}
                    Err(error) => panic!("analysis event timed out: {error}"),
                }
            }
            let deadline = Instant::now() + Duration::from_secs(1);
            while inventories.load(Ordering::SeqCst) < 1 && Instant::now() < deadline {
                std::thread::yield_now();
            }
        }
        // A following command can run only after the second analysis has completed its post-work
        // sweep decision, making the counter assertion deterministic without a timing sleep.
        engine.submit(EngineCommand::Materialize(MaterializeJob {
            token: 9,
            reference: LibraryRef {
                fqn: String::new(),
                member_name: String::new(),
                member_desc: String::new(),
            },
        }));
        loop {
            match incoming.recv_timeout(Duration::from_secs(1)) {
                Ok(Incoming::Engine(EngineEvent::Materialized(MaterializeResult {
                    token: 9,
                    ..
                }))) => break,
                Ok(_) => {}
                Err(error) => panic!("materialization event timed out: {error}"),
            }
        }

        assert_eq!(
            inventories.load(Ordering::SeqCst),
            1,
            "interactive analyses must not re-walk the complete workspace once its sweep is queued"
        );
        engine.join();
    }
}
