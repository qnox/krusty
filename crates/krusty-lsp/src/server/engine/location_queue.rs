//! Location-queue scheduling. Interactive commands and dependency location are separate
//! deques, so draining a location backlog does not scan it.

use super::{
    CommandState, DependencyCandidate, EngineCommand, INTERACTIVE_COMMANDS_BEFORE_LOCATION,
};

impl CommandState {
    /// The query already returned. Foreground dequeue runs interactive work ahead of this queue,
    /// up to [`INTERACTIVE_COMMANDS_BEFORE_LOCATION`], and queued location does not keep the
    /// workspace sweep from being admitted.
    pub(super) fn enqueue_location(
        &mut self,
        generation: u64,
        candidates: Vec<DependencyCandidate>,
    ) {
        self.locations.push_back(EngineCommand::LocateDependencies {
            generation,
            candidates,
        });
    }

    /// The next edit, dump, materialization, or project change. Location stays on its own queue
    /// until none of those are waiting, or until interactive work has already run
    /// [`INTERACTIVE_COMMANDS_BEFORE_LOCATION`] times ahead of it. Two location commands keep the
    /// order they were queued in. Both pops are from the front of a deque.
    pub(super) fn take_foreground(&mut self) -> Option<EngineCommand> {
        let location_due = !self.locations.is_empty()
            && self.interactive_since_location >= INTERACTIVE_COMMANDS_BEFORE_LOCATION;
        if location_due {
            self.interactive_since_location = 0;
            return self.locations.pop_front();
        }
        if let Some(command) = self.pop_interactive() {
            return Some(command);
        }
        self.interactive_since_location = 0;
        self.locations.pop_front()
    }

    /// The next edit, dump, materialization, or project change, leaving location queued.
    ///
    /// Shutdown and an overdue refresh use this so they can finish interactive work and then
    /// stop. A due location yield does not run, and an idle location backlog is not drained.
    pub(super) fn take_interactive(&mut self) -> Option<EngineCommand> {
        self.pop_interactive()
    }

    fn pop_interactive(&mut self) -> Option<EngineCommand> {
        let command = self.pending.pop_front()?;
        if self.locations.is_empty() {
            self.interactive_since_location = 0;
        } else {
            self.interactive_since_location += 1;
        }
        Some(command)
    }

    /// True when an edit, dump, materialization, or project change is waiting. Queued dependency
    /// location is absent from this check, so serving an edit still admits the workspace sweep
    /// while those classes are written afterwards.
    pub(super) fn interactive_work_queued(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Location candidates are classpath identities. Drop them with the model-owned index queues.
    pub(super) fn clear_queued_locations(&mut self) {
        self.locations.clear();
        self.interactive_since_location = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use super::super::{
        command_queue, Analysis, AnalysisEngine, AnalysisJob, CommandReceive, CommandState,
        DependencyCandidate, DumpJob, EngineCommand, LibraryRef, LocatedDependency, MaterializeJob,
        MaterializedDefinition, INTERACTIVE_COMMANDS_BEFORE_LOCATION,
    };
    use crate::{DocumentAnalysis, IndexOutcome};

    fn located(name: &str) -> EngineCommand {
        EngineCommand::LocateDependencies {
            generation: 1,
            candidates: vec![DependencyCandidate {
                internal: format!("vendor/{name}"),
                package: "vendor".to_string(),
                name: name.to_string(),
            }],
        }
    }

    #[test]
    fn a_large_location_backlog_yields_to_an_edit_and_drains_in_order() {
        const BACKLOG: usize = 4096;
        let mut state = CommandState::default();
        for index in 0..BACKLOG {
            state.enqueue(located(&index.to_string()));
        }
        state.enqueue(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///a.kt".into(), "fun a(){}".into(), 2, 0)],
            open_uris: Vec::new(),
        }));

        assert!(
            matches!(state.take(), Some(EngineCommand::Analyze(_))),
            "one edit dequeues ahead of the whole location backlog"
        );
        for index in 0..BACKLOG {
            match state.take() {
                Some(EngineCommand::LocateDependencies { candidates, .. }) => {
                    assert_eq!(candidates[0].name, index.to_string());
                }
                other => panic!("expected location {index} in queue order, got {other:?}"),
            }
        }
        assert!(state.take().is_none());
    }

    #[test]
    fn location_is_served_after_the_interactive_yield_bound() {
        let mut state = CommandState::default();
        state.enqueue(located("Held"));
        for token in 0..INTERACTIVE_COMMANDS_BEFORE_LOCATION {
            state.enqueue(EngineCommand::Dump(DumpJob {
                token: token as u64,
                uri: "file:///a.kt".into(),
            }));
        }
        state.enqueue(EngineCommand::Dump(DumpJob {
            token: 1_000,
            uri: "file:///a.kt".into(),
        }));

        for token in 0..INTERACTIVE_COMMANDS_BEFORE_LOCATION {
            match state.take() {
                Some(EngineCommand::Dump(job)) => assert_eq!(job.token, token as u64),
                other => panic!("expected interactive command {token}, got {other:?}"),
            }
        }
        match state.take() {
            Some(EngineCommand::LocateDependencies { candidates, .. }) => {
                assert_eq!(candidates[0].name, "Held");
            }
            other => panic!("location must run once the yield bound is reached, got {other:?}"),
        }
        match state.take() {
            Some(EngineCommand::Dump(job)) => assert_eq!(job.token, 1_000),
            other => panic!("interactive work resumes after the yielded location, got {other:?}"),
        }
        assert!(state.take().is_none());
    }

    #[test]
    fn an_edit_runs_through_a_queued_location_backlog() {
        const BACKLOG: usize = 64;

        struct Mock {
            entered: Arc<(Mutex<bool>, Condvar)>,
            release: Arc<(Mutex<bool>, Condvar)>,
            order: Arc<Mutex<Vec<String>>>,
            held: bool,
        }
        impl Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                self.order.lock().expect("order").push("analyze".into());
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
            fn locate_dependencies(
                &mut self,
                candidates: Vec<DependencyCandidate>,
            ) -> Vec<LocatedDependency> {
                let name = candidates
                    .first()
                    .map(|candidate| candidate.name.clone())
                    .unwrap_or_default();
                self.order.lock().expect("order").push(name);
                Vec::new()
            }
            fn materialize_library_definition(
                &mut self,
                _reference: &LibraryRef,
            ) -> Option<MaterializedDefinition> {
                if !self.held {
                    self.held = true;
                    let (lock, ready) = &*self.entered;
                    *lock.lock().expect("entered") = true;
                    ready.notify_one();
                    let (lock, ready) = &*self.release;
                    let mut release = lock.lock().expect("release");
                    while !*release {
                        release = ready.wait(release).expect("release");
                    }
                }
                None
            }
        }

        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let order = Arc::new(Mutex::new(Vec::new()));
        let (events, incoming) = sync_channel(64);
        let engine = AnalysisEngine::spawn(
            Mock {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                order: Arc::clone(&order),
                held: false,
            },
            events,
        );
        engine.submit(EngineCommand::Materialize(MaterializeJob {
            token: 1,
            reference: LibraryRef {
                fqn: String::new(),
                member_name: String::new(),
                member_desc: String::new(),
            },
        }));
        {
            let (lock, ready) = &*entered;
            let mut entered = lock.lock().expect("entered");
            while !*entered {
                entered = ready.wait(entered).expect("entered");
            }
        }
        for index in 0..BACKLOG {
            engine.submit(EngineCommand::LocateDependencies {
                generation: 0,
                candidates: vec![DependencyCandidate {
                    internal: format!("vendor/Type{index}"),
                    package: "vendor".into(),
                    name: index.to_string(),
                }],
            });
        }
        engine.submit(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///a.kt".into(), "fun a(){}".into(), 2, 0)],
            open_uris: Vec::new(),
        }));
        {
            let (lock, ready) = &*release;
            *lock.lock().expect("release") = true;
            ready.notify_one();
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            while incoming.try_recv().is_ok() {}
            let order = order.lock().expect("order");
            if order.len() == 1 + BACKLOG {
                assert_eq!(order[0], "analyze");
                for (index, name) in order.iter().skip(1).enumerate() {
                    assert_eq!(name, &index.to_string());
                }
                break;
            }
            if Instant::now() >= deadline {
                panic!("edit did not run ahead of the location backlog: {order:?}");
            }
            drop(order);
            std::thread::yield_now();
        }
        engine.join();
    }

    #[test]
    fn an_edit_runs_ahead_of_queued_dependency_location() {
        let mut state = CommandState::default();
        state.enqueue(EngineCommand::Dump(DumpJob {
            token: 7,
            uri: "file:///a.kt".into(),
        }));
        state.enqueue(located("First"));
        state.enqueue(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///a.kt".into(), "fun a(){}".into(), 2, 0)],
            open_uris: Vec::new(),
        }));
        state.enqueue(located("Second"));

        assert!(state.interactive_work_queued());
        assert!(matches!(state.take(), Some(EngineCommand::Dump(_))));
        assert!(matches!(state.take(), Some(EngineCommand::Analyze(_))));
        assert!(
            !state.interactive_work_queued(),
            "the two location commands still queued are not interactive work"
        );
        match state.take() {
            Some(EngineCommand::LocateDependencies { candidates, .. }) => {
                assert_eq!(candidates[0].name, "First");
            }
            other => panic!("expected the earlier location, got {other:?}"),
        }
        match state.take() {
            Some(EngineCommand::LocateDependencies { candidates, .. }) => {
                assert_eq!(candidates[0].name, "Second");
            }
            other => panic!("expected the later location, got {other:?}"),
        }
        assert!(state.take().is_none());
    }

    #[test]
    fn queued_dependency_location_does_not_hold_back_the_sweep() {
        let mut state = CommandState::default();
        state.enqueue(located("Pending"));
        assert!(
            !state.interactive_work_queued(),
            "location is not interactive work, so an edit that just finished can admit the sweep"
        );
        state.enqueue(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///a.kt".into(), String::new(), 1, 0)],
            open_uris: Vec::new(),
        }));
        assert!(state.interactive_work_queued());
    }

    #[test]
    fn an_expired_refresh_deadline_runs_before_queued_location() {
        let (sender, receiver) = command_queue();
        sender.send(located("Held"));
        for token in 0..INTERACTIVE_COMMANDS_BEFORE_LOCATION {
            sender.send(EngineCommand::Dump(DumpJob {
                token: token as u64,
                uri: "file:///a.kt".into(),
            }));
        }
        sender.send(EngineCommand::Dump(DumpJob {
            token: 1_000,
            uri: "file:///a.kt".into(),
        }));

        let mut order = Vec::new();
        for token in 0..=INTERACTIVE_COMMANDS_BEFORE_LOCATION {
            match receiver.recv(Some(Duration::ZERO)) {
                CommandReceive::Command(EngineCommand::Dump(job)) => order.push(job.token),
                CommandReceive::Command(_) => {
                    panic!("expected dump {token} before location")
                }
                CommandReceive::Timeout => {
                    panic!("expected dump {token} before the refresh deadline")
                }
                CommandReceive::Disconnected => panic!("expected dump {token} before disconnect"),
            }
        }
        assert_eq!(
            order,
            (0..INTERACTIVE_COMMANDS_BEFORE_LOCATION as u64)
                .chain(std::iter::once(1_000))
                .collect::<Vec<_>>()
        );
        assert!(
            matches!(receiver.recv(Some(Duration::ZERO)), CommandReceive::Timeout),
            "an overdue refresh runs before a due location yield"
        );
    }
}
