use super::LspService;
use crate::server::engine::{AnalysisBatch, AnalyzedSnapshot};
use crate::DocumentAnalysis;

#[test]
fn fresh_batch_keeps_analyzed_text_not_the_live_buffer() {
    let mut service = LspService::new(|sources: &[&str]| {
        sources
            .iter()
            .map(|_| DocumentAnalysis::empty())
            .collect::<Vec<_>>()
    });
    service.open_document_for_test("file:///a.kt", "fun live() {}", 1);
    service.open_document_for_test("file:///b.kt", "fun other() {}", 1);
    AnalyzedSnapshot::install(vec![
        "fun analyzed() {}".to_string(),
        "fun dependency() {}".to_string(),
    ]);

    service.apply_analysis_batch(AnalysisBatch {
        analyzed: vec![("file:///a.kt".into(), 1), ("file:///b.kt".into(), 1)],
        analyses: vec![DocumentAnalysis::empty(), DocumentAnalysis::empty()],
        support_documents: vec![("file:///support.kt".into(), "fun support() {}".into())],
        pending: false,
    });

    assert_eq!(service.documents["file:///a.kt"].text, "fun live() {}");
    assert_eq!(
        service.source_set,
        vec![
            ("file:///a.kt".into(), "fun analyzed() {}".into()),
            ("file:///b.kt".into(), "fun dependency() {}".into()),
            ("file:///support.kt".into(), "fun support() {}".into()),
        ]
    );
}
