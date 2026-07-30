//! Analysis worker for the LSP request loop.

use std::collections::VecDeque;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::super::{DocumentAnalysis, MaterializedDefinition};
use super::implementation::{
    Analysis, AnalysisBackend, DocumentAdmission, Incoming, ProjectFeedback,
};
use crate::compiler_analysis::LibraryRef;

const MAX_PENDING_WATCHED_FILES: usize = 1024;

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
            EngineCommand::SetWorkspaceRoot(root) => {
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
                if result.timed_out() && state.pending.is_empty() {
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
}
