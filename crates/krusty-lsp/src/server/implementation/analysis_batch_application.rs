//! Apply one analysis completion to the open documents and the navigation snapshot.
//!
//! The batch already paired each result with the URI, version, and text the worker saw. This
//! module only installs that record, or publishes an incomplete-result diagnostic when the worker
//! returned a different number of analyses than documents.

use std::time::Duration;

use serde_json::Value;

use crate::DocumentAnalysis;
use crate::WorkspaceSymbolIndex;
use krusty::diag::{Diagnostic, DiagnosticKind, Severity};

use super::{publish_diagnostics, AnalysisBackend, DiagnosticBudget, DiagnosticIndex, LspService};

impl<B> LspService<B>
where
    B: AnalysisBackend,
{
    pub(crate) fn apply_analysis_batch(
        &mut self,
        batch: crate::server::engine::AnalysisBatch,
    ) -> Vec<Value> {
        self.analysis_in_flight = false;
        let resubmit = std::mem::take(&mut self.resubmit_pending);
        let changed = std::mem::take(&mut self.changed_identities);
        let pending = batch.is_pending();
        let complete = batch.is_complete();
        let fresh = batch
            .documents()
            .iter()
            .map(|document| {
                !changed.contains(document.uri())
                    && self
                        .documents
                        .get(document.uri())
                        .is_some_and(|open| open.version == document.version())
            })
            .collect::<Vec<_>>();
        if fresh.iter().any(|fresh| !fresh) {
            self.analysis_dirty = true;
        }
        if !fresh.is_empty() && !fresh.iter().any(|fresh| *fresh) {
            return Vec::new();
        }
        let batch_is_fresh = fresh.iter().all(|fresh| *fresh);
        let uris = batch
            .documents()
            .iter()
            .zip(&fresh)
            .filter(|(_, fresh)| **fresh)
            .map(|(document, _)| document.uri().to_owned())
            .collect::<Vec<_>>();
        let (documents, support_documents) = batch.into_documents();
        if pending {
            let current_uris = self
                .analyzed_uris()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            self.schedule_analysis_retry(&current_uris);
            return Vec::new();
        }
        let mut diagnostic_budget = DiagnosticBudget::default();
        if !complete {
            if !batch_is_fresh {
                self.analysis_dirty = true;
                return Vec::new();
            }
            self.source_set.clear();
            self.workspace_symbols = WorkspaceSymbolIndex::default();
            for uri in &uris {
                let open = self
                    .documents
                    .get_mut(uri)
                    .expect("batch freshness checked before applying");
                open.clear_analysis();
                open.diagnostics = DiagnosticIndex::from_diagnostics(
                    vec![Diagnostic {
                        span: krusty::diag::Span::new(0, 0),
                        editor_span: None,
                        identity: None,
                        severity: Severity::Error,
                        kind: DiagnosticKind::Compiler,
                        msg: "analysis worker returned an incomplete source set".to_string(),
                        file: 0,
                    }],
                    &open.text,
                    &mut diagnostic_budget,
                );
            }
            if resubmit {
                self.analysis_dirty = true;
            }
            let mut messages = uris
                .into_iter()
                .filter_map(|uri| {
                    let open = &self.documents[&uri];
                    self.publish(&uri, Some(open.version), &open.diagnostics)
                })
                .collect::<Vec<_>>();
            messages.extend(self.diagnostic_refresh());
            if !resubmit {
                messages.extend(self.complete_pending_analysis_requests());
            }
            return messages;
        }
        self.analysis_retry_at = None;
        self.analysis_retry_backoff = Duration::ZERO;
        let push = self.pushes_diagnostics();
        let mut messages = Vec::with_capacity(documents.len());
        let mut analyzed_documents = Vec::with_capacity(documents.len());
        let mut workspace_symbols = WorkspaceSymbolIndex::default();
        for (document, fresh) in documents.into_iter().zip(fresh) {
            if !fresh {
                continue;
            }
            let (uri, text, analysis) = document.into_result();
            let Some(analysis) = analysis else {
                continue;
            };
            let DocumentAnalysis {
                diagnostics,
                hover,
                completion,
                signature_help,
                semantic_tokens,
                definitions,
                type_definitions,
                implementations,
                library_definitions,
                document_symbols,
                workspace_symbols: document_workspace_symbols,
                folding_ranges,
                implementation_relations: _,
            } = analysis;
            if batch_is_fresh {
                workspace_symbols.merge_from(document_workspace_symbols);
            }
            let open = self
                .documents
                .get_mut(&uri)
                .expect("batch freshness checked before applying");
            open.hover = hover;
            open.completion = completion;
            open.signature_help = signature_help;
            open.semantic_tokens = semantic_tokens;
            open.library_definitions = library_definitions;
            open.document_symbols = document_symbols;
            open.folding_ranges = folding_ranges;
            if batch_is_fresh {
                open.definitions = definitions;
                open.type_definitions = type_definitions;
                open.implementations = implementations;
            }
            open.diagnostics =
                DiagnosticIndex::from_diagnostics(diagnostics, &open.text, &mut diagnostic_budget);
            if push {
                messages.push(publish_diagnostics(
                    &uri,
                    Some(open.version),
                    &open.diagnostics,
                ));
            }
            if batch_is_fresh {
                let text = text.unwrap_or_else(|| open.text.clone());
                analyzed_documents.push((uri.clone(), text));
            }
        }
        if batch_is_fresh {
            self.source_set = analyzed_documents
                .into_iter()
                .chain(support_documents)
                .collect();
            // Bind each source-set slot to its URI before the index records those positions.
            let uris = self
                .source_set
                .iter()
                .map(|(uri, _)| uri.as_str())
                .collect::<Vec<_>>();
            workspace_symbols.assign_uris(&uris);
            self.workspace_symbols = workspace_symbols;
        }
        messages.extend(self.diagnostic_refresh());
        if resubmit {
            self.analysis_dirty = true;
        }
        if !self.analysis_dirty {
            messages.extend(self.complete_pending_analysis_requests());
        }
        messages
    }
}
