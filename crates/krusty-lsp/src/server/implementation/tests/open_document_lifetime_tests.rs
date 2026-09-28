use super::*;

#[test]
fn close_reopen_with_reused_version_discards_in_flight_batch() {
    let mut service = LspService::new(|sources: &[&str]| {
        sources
            .iter()
            .map(|_| DocumentAnalysis::empty())
            .collect::<Vec<_>>()
    });
    service.force_initialized_for_test();

    let with_diagnostic = || DocumentAnalysis {
        diagnostics: vec![Diagnostic {
            span: krusty::diag::Span::new(0, 0),
            editor_span: None,
            identity: None,
            severity: Severity::Error,
            kind: DiagnosticKind::Compiler,
            msg: "from stale batch".to_string(),
            file: 0,
        }],
        ..DocumentAnalysis::empty()
    };
    let did_open = |uri: &str, text: &str, version: i64| {
        serde_json::json!({
            "textDocument": { "uri": uri, "languageId": "kotlin", "version": version, "text": text }
        })
    };

    service.did_open(None, did_open("file:///a.kt", "old", 1), true);
    let in_flight_job = service
        .dispatch_pending_analysis()
        .expect("job for the freshly opened document");
    assert_eq!(in_flight_job.documents[0].2, 1);
    assert!(service.analysis_in_flight_for_test());

    service.did_close(
        None,
        serde_json::json!({ "textDocument": { "uri": "file:///a.kt" } }),
        true,
    );
    service.did_open(None, did_open("file:///a.kt", "new", 1), true);

    let stale_batch = AnalysisBatch {
        analyzed: vec![("file:///a.kt".into(), 1)],
        analyses: vec![with_diagnostic()],
        support_documents: Vec::new(),
        pending: false,
    };
    let messages = service.apply_analysis_batch(stale_batch);
    assert!(
        messages.is_empty(),
        "stale close+reopen batch must not publish diagnostics"
    );
    assert_eq!(
        service.document_diagnostic_count_for_test("file:///a.kt"),
        0,
        "stale analysis must not populate the reopened document's indices"
    );
    assert!(!service.analysis_in_flight_for_test());
    assert!(service.analysis_dirty_for_test());
    assert!(
        !service.resubmit_pending_for_test(),
        "discard clears the resubmit slot; a fresh job re-dispatches via analysis_dirty"
    );

    let fresh_job = service
        .dispatch_pending_analysis()
        .expect("fresh job re-dispatches after discard");
    assert_eq!(fresh_job.documents[0].1, "new");
    assert_ne!(
        in_flight_job.documents[0].3, fresh_job.documents[0].3,
        "reopen assigns a new document lifetime"
    );

    struct GroupHash {
        hashes: std::rc::Rc<std::cell::RefCell<Vec<u64>>>,
    }
    impl Analysis for GroupHash {
        fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
            sources.iter().map(|_| DocumentAnalysis::empty()).collect()
        }

        fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
            IndexOutcome::default()
        }

        fn analyze_open_documents(
            &mut self,
            documents: &[(&str, &str)],
            _open_uris: &[&str],
        ) -> (Vec<DocumentAnalysis>, Vec<(String, crate::SharedSource)>) {
            self.hashes.borrow_mut().extend(
                documents
                    .iter()
                    .map(|(uri, text)| crate::open_document_digest::text_hash(uri, text)),
            );
            let sources = documents.iter().map(|(_, text)| *text).collect::<Vec<_>>();
            (self.analyze(&sources), Vec::new())
        }
    }
    let hashes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut backend = InlineBackend::new(GroupHash {
        hashes: hashes.clone(),
    });
    let replay = AnalysisJob {
        documents: fresh_job.documents.clone(),
        open_uris: fresh_job.open_uris.clone(),
    };
    backend
        .submit(in_flight_job)
        .expect("inline analysis installs the in-flight lifetime");
    backend
        .submit(fresh_job)
        .expect("inline analysis installs the reopened lifetime");
    backend
        .submit(replay)
        .expect("the same reopened job can be analyzed again");
    let hashes = hashes.borrow();
    assert_eq!(hashes.len(), 3);
    assert_ne!(hashes[0], hashes[1], "reopen hashes the replacement text");
    assert_eq!(hashes[1], hashes[2], "the reopened job is stable");

    let fresh_batch = AnalysisBatch {
        analyzed: vec![("file:///a.kt".into(), 1)],
        analyses: vec![with_diagnostic()],
        support_documents: Vec::new(),
        pending: false,
    };
    let messages = service.apply_analysis_batch(fresh_batch);
    assert_eq!(messages.len(), 1, "fresh batch is applied");
    assert_eq!(messages[0]["method"], "textDocument/publishDiagnostics");
    assert_eq!(
        service.document_diagnostic_count_for_test("file:///a.kt"),
        1,
        "fresh analysis populates the reopened document"
    );
}
