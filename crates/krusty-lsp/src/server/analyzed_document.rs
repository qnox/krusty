//! One analyzed open document: the URI, version, and text the worker saw, with its result.
//!
//! The completion payload owns that record. A worker that returns a different number of analyses
//! than documents marks the batch incomplete, so a missing or extra result cannot be paired with
//! the wrong buffer.

use crate::DocumentAnalysis;

/// The open document an analysis completion is about.
pub struct AnalyzedDocument {
    pub uri: String,
    pub version: i64,
    /// Text the analysis thread owned for this URI and version.
    ///
    /// `Some` is the buffer that was analyzed, including `Some("")` for an empty file. `None`
    /// means the caller did not carry a buffer; applying the batch uses the live editor text.
    pub text: Option<String>,
    /// Absent for every document when the worker's result count does not match.
    pub analysis: Option<DocumentAnalysis>,
}

/// A completed analysis of one open-document set.
pub struct AnalysisBatch {
    pub documents: Vec<AnalyzedDocument>,
    /// `false` when `analyses` and `documents` had different lengths. Every analysis is then absent.
    pub complete: bool,
    pub support_documents: Vec<(String, String)>,
    pub pending: bool,
}

impl AnalysisBatch {
    /// Pair each job document with the analysis at the same position.
    ///
    /// `documents` are `(uri, text, version)` in job order. A length mismatch drops every analysis
    /// rather than attaching a result to a different document.
    pub fn from_job(
        documents: Vec<(String, String, i64)>,
        analyses: Vec<DocumentAnalysis>,
        support_documents: Vec<(String, String)>,
        pending: bool,
    ) -> Self {
        let complete = analyses.len() == documents.len();
        let mut analyses = analyses.into_iter();
        let documents = documents
            .into_iter()
            .map(|(uri, text, version)| AnalyzedDocument {
                uri,
                version,
                text: Some(text),
                analysis: if complete { analyses.next() } else { None },
            })
            .collect();
        Self {
            documents,
            complete,
            support_documents,
            pending,
        }
    }

    /// Pair each URI and version with the analysis at the same position, with no carried text.
    ///
    /// Callers that did not retain the buffer use this. Applying the batch falls back to the live
    /// editor text, which is distinct from an analyzed empty file. A length mismatch still drops
    /// every analysis, the same as [`Self::from_job`].
    pub fn from_versions(
        documents: Vec<(String, i64)>,
        analyses: Vec<DocumentAnalysis>,
        support_documents: Vec<(String, String)>,
        pending: bool,
    ) -> Self {
        let complete = analyses.len() == documents.len();
        let mut analyses = analyses.into_iter();
        let documents = documents
            .into_iter()
            .map(|(uri, version)| AnalyzedDocument {
                uri,
                version,
                text: None,
                analysis: if complete { analyses.next() } else { None },
            })
            .collect();
        Self {
            documents,
            complete,
            support_documents,
            pending,
        }
    }

    pub fn versions(&self) -> Vec<(String, i64)> {
        self.documents
            .iter()
            .map(|document| (document.uri.clone(), document.version))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::AnalysisBatch;
    use crate::server::implementation::LspService;
    use crate::DocumentAnalysis;
    use krusty::diag::{Diagnostic, DiagnosticKind, Severity};

    fn diagnostic(message: &str) -> DocumentAnalysis {
        DocumentAnalysis::with_diagnostics(vec![Diagnostic {
            span: krusty::diag::Span::new(0, 0),
            editor_span: None,
            identity: None,
            severity: Severity::Error,
            kind: DiagnosticKind::Compiler,
            msg: message.to_string(),
            file: 0,
        }])
    }

    #[test]
    fn versions_without_source_text_stay_paired_with_each_analysis() {
        let batch = AnalysisBatch::from_versions(
            vec![("file:///a.kt".into(), 3)],
            vec![diagnostic("boom")],
            Vec::new(),
            false,
        );
        assert!(batch.complete);
        assert_eq!(batch.documents[0].uri, "file:///a.kt");
        assert_eq!(batch.documents[0].version, 3);
        assert!(batch.documents[0].text.is_none());
        assert!(batch.documents[0].analysis.is_some());
        assert_eq!(batch.versions(), vec![("file:///a.kt".into(), 3)]);
    }

    #[test]
    fn a_length_mismatch_drops_every_analysis() {
        let short = AnalysisBatch::from_job(
            vec![
                ("file:///a.kt".into(), "fun a() {}".into(), 1),
                ("file:///b.kt".into(), "fun b() {}".into(), 1),
            ],
            vec![diagnostic("boom")],
            Vec::new(),
            false,
        );
        assert!(!short.complete);
        assert!(short
            .documents
            .iter()
            .all(|document| document.analysis.is_none()));

        let extra = AnalysisBatch::from_job(
            vec![("file:///a.kt".into(), "fun a() {}".into(), 1)],
            vec![diagnostic("one"), diagnostic("two")],
            Vec::new(),
            false,
        );
        assert!(!extra.complete);
        assert!(extra.documents[0].analysis.is_none());
    }

    #[test]
    fn a_fresh_batch_keeps_each_documents_analyzed_text() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.open_document_for_test("file:///a.kt", "fun live() {}", 1);
        service.open_document_for_test("file:///b.kt", "fun other() {}", 1);
        service.apply_analysis_batch(AnalysisBatch::from_job(
            vec![
                ("file:///a.kt".into(), "fun analyzed() {}".into(), 1),
                ("file:///b.kt".into(), "fun dependency() {}".into(), 1),
            ],
            vec![DocumentAnalysis::empty(), DocumentAnalysis::empty()],
            vec![("file:///support.kt".into(), "fun support() {}".into())],
            false,
        ));

        assert_eq!(
            service.open_text_for_test("file:///a.kt"),
            Some("fun live() {}")
        );
        assert_eq!(
            service.source_set_for_test(),
            &[
                ("file:///a.kt".into(), "fun analyzed() {}".into()),
                ("file:///b.kt".into(), "fun dependency() {}".into()),
                ("file:///support.kt".into(), "fun support() {}".into()),
            ]
        );
    }

    #[test]
    fn a_stale_completion_does_not_replace_the_snapshot_a_fresh_one_writes() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.open_document_for_test("file:///a.kt", "fun live() {}", 1);
        service.apply_analysis_batch(AnalysisBatch::from_job(
            vec![("file:///a.kt".into(), "fun analyzed() {}".into(), 1)],
            vec![DocumentAnalysis::empty()],
            Vec::new(),
            false,
        ));

        service.open_document_for_test("file:///a.kt", "fun edited() {}", 2);
        let stale = service.apply_analysis_batch(AnalysisBatch::from_job(
            vec![("file:///a.kt".into(), "fun stale() {}".into(), 1)],
            vec![diagnostic("stale")],
            Vec::new(),
            false,
        ));
        assert!(stale.is_empty());
        assert_eq!(
            service.source_set_for_test(),
            &[("file:///a.kt".into(), "fun analyzed() {}".into())]
        );

        service.apply_analysis_batch(AnalysisBatch::from_job(
            vec![("file:///a.kt".into(), "fun current() {}".into(), 2)],
            vec![DocumentAnalysis::empty()],
            Vec::new(),
            false,
        ));
        assert_eq!(
            service.open_text_for_test("file:///a.kt"),
            Some("fun edited() {}")
        );
        assert_eq!(
            service.source_set_for_test(),
            &[("file:///a.kt".into(), "fun current() {}".into())]
        );
    }

    #[test]
    fn an_incomplete_worker_result_is_not_paired_with_the_first_document() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "fun a() {}", 1);
        service.open_document_for_test("file:///b.kt", "fun b() {}", 1);
        let messages = service.apply_analysis_batch(AnalysisBatch::from_job(
            vec![
                ("file:///a.kt".into(), "fun a() {}".into(), 1),
                ("file:///b.kt".into(), "fun b() {}".into(), 1),
            ],
            vec![diagnostic("boom")],
            Vec::new(),
            false,
        ));
        let published = messages
            .iter()
            .filter_map(|message| message["params"]["diagnostics"][0]["message"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            published,
            vec![
                "Analysis worker returned an incomplete source set",
                "Analysis worker returned an incomplete source set",
            ]
        );
        assert!(service.source_set_for_test().is_empty());
    }

    #[test]
    fn absent_text_uses_the_live_buffer_and_analyzed_empty_text_stays_empty() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.open_document_for_test("file:///a.kt", "fun live() {}", 1);
        service.apply_analysis_batch(AnalysisBatch::from_versions(
            vec![("file:///a.kt".into(), 1)],
            vec![DocumentAnalysis::empty()],
            Vec::new(),
            false,
        ));
        assert_eq!(
            service.source_set_for_test(),
            &[("file:///a.kt".into(), "fun live() {}".into())]
        );

        service.apply_analysis_batch(AnalysisBatch::from_job(
            vec![("file:///a.kt".into(), String::new(), 1)],
            vec![DocumentAnalysis::empty()],
            Vec::new(),
            false,
        ));
        assert_eq!(
            service.source_set_for_test(),
            &[("file:///a.kt".into(), String::new())]
        );
    }
}
