//! Analysis worker for the LSP request loop.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::super::{DocumentAnalysis, IndexedFile, MaterializedDefinition};
use super::implementation::{
    Analysis, AnalysisBackend, DocumentAdmission, Incoming, ProjectFeedback,
};
use crate::compiler_analysis::LibraryRef;

const MAX_PENDING_WATCHED_FILES: usize = 1024;
/// The longest an interactive command can be made to wait: one chunk of index work. Sized to sit
/// inside a single worker source-set round trip.
const MAX_INDEX_CHUNK_FILES: usize = 32;
/// Ceiling on files awaiting indexing, so a pathological workspace cannot grow the queue without
/// bound. Reaching it drops the excess; the sweep re-offers those files on its next pass.
const MAX_QUEUED_INDEX_FILES: usize = 200_000;
/// Companion byte ceiling. A count alone is not a memory bound: deeply nested workspaces produce
/// long URIs, and each is retained twice, in a chunk and in the priority map.
const MAX_QUEUED_INDEX_BYTES: usize = 16 * 1024 * 1024;

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

#[derive(Debug)]
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
    queued: HashMap<String, IndexPriority>,
    queued_bytes: usize,
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
            EngineCommand::Index(job) => {
                let priority = job.priority;
                let mut chunk = Vec::with_capacity(MAX_INDEX_CHUNK_FILES.min(job.uris.len()));
                for uri in job.uris {
                    if self.queued.len() >= MAX_QUEUED_INDEX_FILES
                        || self.queued_bytes >= MAX_QUEUED_INDEX_BYTES
                    {
                        break;
                    }
                    match self.queued.get(&uri) {
                        // Already waiting at this level or a higher one; nothing to do.
                        Some(IndexPriority::Neighborhood) => continue,
                        Some(IndexPriority::Sweep) if priority == IndexPriority::Sweep => continue,
                        // Promotion: re-queue at the higher level and let the sweep entry lapse.
                        Some(IndexPriority::Sweep) => {
                            self.queued.insert(uri.clone(), priority);
                        }
                        None => {
                            self.queued_bytes = self.queued_bytes.saturating_add(uri.len());
                            self.queued.insert(uri.clone(), priority);
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
                self.discard_index_work();
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
            return Some(EngineCommand::Index(job));
        }
    }

    /// Drop the URIs this chunk no longer owns, then release the rest so a file that changes while
    /// its chunk waits can be offered again.
    fn claim(&mut self, mut job: IndexJob) -> Option<IndexJob> {
        job.uris
            .retain(|uri| self.queued.get(uri) == Some(&job.priority));
        if job.uris.is_empty() {
            return None;
        }
        for uri in &job.uris {
            if self.queued.remove(uri).is_some() {
                self.queued_bytes = self.queued_bytes.saturating_sub(uri.len());
            }
        }
        Some(job)
    }

    fn indexing_outstanding(&self) -> bool {
        !self.neighborhood.is_empty() || !self.sweep.is_empty()
    }

    /// Work queued against the previous model must never run against its replacement.
    fn discard_index_work(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.neighborhood.clear();
        self.sweep.clear();
        self.queued.clear();
        self.queued_bytes = 0;
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
    let mut indexing = false;
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
            Some(EngineCommand::Index(job)) if !indexing => {
                indexing = true;
                Some(format!("Indexing {} files", job.uris.len()))
            }
            Some(EngineCommand::Index(_)) => None,
            _ => None,
        };
        if let Some(message) = working {
            if send_status(&events, ServerStatus::Working(message)).is_err() {
                break;
            }
        } else if !matches!(command, Some(EngineCommand::Index(_))) {
            // Any other command ends with Ready, which closes the shared progress token. Clearing
            // the latch here means the next chunk re-announces instead of indexing silently.
            indexing = false;
        }
        match command {
            None => {
                let feedback = analyze_refresh(&mut analyze);
                update_admission(&admission, analyze.document_admission());
                if emit_project(&events, &mut analyze, feedback, &mut last_ready).is_err() {
                    break;
                }
            }
            Some(EngineCommand::SetWorkspaceRoot(root)) => {
                let feedback = analyze.set_workspace_root(root);
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
                    submit_workspace_sweep(&mut analyze, &commands);
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
                if !outstanding {
                    indexing = false;
                    if send_status(&events, ServerStatus::Ready).is_err() {
                        break;
                    }
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
                for uri in &uris {
                    reanalyze |= analyze.note_watched_file_change(uri);
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

/// Raise a sweep over everything the project model knows about. Enqueueing is idempotent: files
/// already waiting are skipped, so repeating this after a refresh costs a map lookup per file.
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
            state.take().is_none(),
            "the superseded sweep entry must not be indexed a second time"
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
        let long = "x".repeat(1024);
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
}
