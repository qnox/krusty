//! The resident ceiling replaces the live worker before the next request.

use std::sync::atomic::{AtomicUsize, Ordering};

use krusty_lsp::{
    AnalysisWorker, LspOptions, ResidentSample, WorkerResidentPolicy, DEFAULT_WORKER_RSS_BYTES,
};

static SAMPLE: AtomicUsize = AtomicUsize::new(0);

fn scripted_sample(_pid: u32) -> ResidentSample {
    match SAMPLE.fetch_add(1, Ordering::SeqCst) {
        0 => ResidentSample::Bytes(DEFAULT_WORKER_RSS_BYTES),
        1 => ResidentSample::Bytes(DEFAULT_WORKER_RSS_BYTES + 1),
        _ => ResidentSample::Unavailable,
    }
}

#[test]
fn resident_ceiling_replaces_the_worker_before_the_next_request() {
    SAMPLE.store(0, Ordering::SeqCst);
    let options = LspOptions::parse(std::iter::empty::<String>()).expect("default options");
    let mut worker = AnalysisWorker::spawn_with_resident_policy(
        env!("CARGO_BIN_EXE_krusty-lsp").into(),
        options.effective_classpath(),
        WorkerResidentPolicy::observe(DEFAULT_WORKER_RSS_BYTES, scripted_sample),
    )
    .expect("analysis worker");

    let source = ["fun answer(): Int = 42\n"];
    let at_limit = worker.analyze(&source).expect("request at the ceiling");
    let retained = worker.process_id();
    assert!(at_limit[0].diagnostics.is_empty());

    let over = worker.analyze(&source).expect("request over the ceiling");
    let replaced = worker.process_id();
    assert_ne!(
        retained, replaced,
        "crossing the ceiling replaces the child before the request"
    );
    assert!(over[0].diagnostics.is_empty());

    let unread = worker
        .analyze(&source)
        .expect("request after an unreadable sample");
    assert_eq!(
        worker.process_id(),
        replaced,
        "an unreadable sample keeps the replacement worker"
    );
    assert!(unread[0].diagnostics.is_empty());
    assert_eq!(SAMPLE.load(Ordering::SeqCst), 3);
}
