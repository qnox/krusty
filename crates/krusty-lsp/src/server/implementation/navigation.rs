//! Go-to-definition and find-references for one session.
//!
//! Targets that were bound to a URI keep that identity after the source set that numbered them is
//! gone. A retained closed-file use also keeps the declaration's name text. A later edit can place
//! a different declaration on the same bytes; the use matches only while that text is still the
//! recorded name.

use std::collections::HashSet;

use serde_json::{json, Value};

use super::super::line_index::byte_offset_to_position;
use super::super::response_page::{array_messages, location_messages, partial_result_token};
use super::super::workspace_references::WorkspaceReferenceHit;
use super::{
    invalid_params, rpc_error, rpc_result, AnalysisBackend, Dispatch, LspService, ReferenceParams,
    TextDocumentPositionParams,
};
use crate::compiler_analysis::DefinitionTarget;
use crate::server::engine::MaterializeJob;
use crate::{DeclarationIdentity, DefinitionIndex};
use krusty::diag::Span;

impl<B> LspService<B>
where
    B: AnalysisBackend,
{
    pub(super) fn definition(&mut self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let progress = match partial_result_token(&params) {
            Ok(token) => token,
            Err(()) => return invalid_params(Some(id)),
        };
        let Ok(params) = serde_json::from_value::<TextDocumentPositionParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = open.offset_at(params.position) else {
            return invalid_params(Some(id));
        };
        let locations = self.navigation_locations(&open.definitions, offset);
        let library_ref = locations
            .is_empty()
            .then(|| open.library_definitions.get(offset).cloned())
            .flatten();
        if !locations.is_empty() || library_ref.is_none() {
            return Dispatch::messages(array_messages(id, locations, progress.as_ref()));
        }
        const MAX_PENDING_MATERIALIZATIONS: usize = 128;
        if self.pending_materializations.len() >= MAX_PENDING_MATERIALIZATIONS {
            return Dispatch::messages(vec![rpc_error(
                id,
                -32000,
                "too many pending dependency definitions",
            )]);
        }
        let token = self.next_materialize_token;
        self.next_materialize_token = self.next_materialize_token.wrapping_add(1);
        let result = self.backend.materialize(MaterializeJob {
            token,
            reference: library_ref.unwrap(),
        });
        match result {
            Some(result) => Dispatch::messages(vec![self.materialize_response(id, result)]),
            None => {
                self.pending_materializations.insert(token, id);
                Dispatch::none()
            }
        }
    }

    pub(super) fn type_definition(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let progress = match partial_result_token(&params) {
            Ok(token) => token,
            Err(()) => return invalid_params(Some(id)),
        };
        let Ok(params) = serde_json::from_value::<TextDocumentPositionParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = open.offset_at(params.position) else {
            return invalid_params(Some(id));
        };
        let locations = self.navigation_locations(&open.type_definitions, offset);
        Dispatch::messages(location_messages(id, locations, progress.as_ref()))
    }

    pub(super) fn implementation(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let progress = match partial_result_token(&params) {
            Ok(token) => token,
            Err(()) => return invalid_params(Some(id)),
        };
        let Ok(params) = serde_json::from_value::<TextDocumentPositionParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = open.offset_at(params.position) else {
            return invalid_params(Some(id));
        };
        let locations = self.navigation_locations(&open.implementations, offset);
        Dispatch::messages(location_messages(id, locations, progress.as_ref()))
    }

    fn navigation_locations(&self, index: &DefinitionIndex, offset: u32) -> Vec<Value> {
        index
            .get(offset)
            .filter_map(|target| self.target_location(index, target))
            .collect()
    }

    /// Resolve one definition target. A bound URI is matched against open text first and then
    /// against the project symbol index, so a declaration that lives in another source set — or
    /// only in a closed-file chunk — is still a location. An unbound target stays a file index
    /// into the current source set.
    fn target_location(&self, index: &DefinitionIndex, target: DefinitionTarget) -> Option<Value> {
        if let Some(uri) = index.file_uri(target.file).map(str::to_owned) {
            if let Some(source) = self.text_for_uri(&uri) {
                return Some(protocol_location(&uri, source, target.span));
            }
            let range = self.project_symbols.declaration_range(&uri, target.span)?;
            return Some(protocol_range(&uri, range));
        }
        let (uri, source) = self.source_set.get(target.file as usize)?;
        Some(protocol_location(uri, source, target.span))
    }

    fn text_for_uri(&self, uri: &str) -> Option<&str> {
        if let Some(open) = self.documents.get(uri) {
            return Some(open.text.as_str());
        }
        self.source_set
            .iter()
            .find(|(candidate, _)| candidate == uri)
            .map(|(_, source)| source.as_str())
    }

    pub(super) fn references(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let progress = match partial_result_token(&params) {
            Ok(token) => token,
            Err(()) => return invalid_params(Some(id)),
        };
        let Ok(params) = serde_json::from_value::<ReferenceParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = open.offset_at(params.position) else {
            return invalid_params(Some(id));
        };
        let targets = open.definitions.get(offset).collect::<Vec<_>>();
        if targets.is_empty() {
            return Dispatch::messages(vec![rpc_result(id, json!([]))]);
        }
        // Every target has to carry an identity. A mix of bound and unbound targets still belongs
        // to one source set, and the file-index search is what that source set can answer.
        let mut identities = HashSet::new();
        let mut unbound = false;
        for target in &targets {
            match open.definitions.declaration_identity(*target) {
                Some(identity) => {
                    identities.insert(identity);
                }
                None => unbound = true,
            }
        }
        let locations = if unbound || identities.is_empty() {
            self.reference_locations_by_file_index(&targets, params.context.include_declaration)
        } else {
            self.reference_locations_by_identity(&identities, params.context.include_declaration)
        };
        Dispatch::messages(array_messages(id, locations, progress.as_ref()))
    }

    /// References recorded before declaration identities existed: targets are file indexes in the
    /// current source set, and only open documents are searched.
    fn reference_locations_by_file_index(
        &self,
        targets: &[DefinitionTarget],
        include_declaration: bool,
    ) -> Vec<Value> {
        let target_ids = targets.iter().copied().collect::<HashSet<_>>();
        let uris = self.analyzed_uris();
        let mut occurrences = Vec::new();
        if include_declaration {
            occurrences.extend(target_ids.iter().filter_map(|target| {
                let (uri, _) = self.source_set.get(target.file as usize)?;
                Some((uri.as_str(), target.span))
            }));
        }
        for (source_file, uri) in uris.iter().enumerate() {
            let document = &self.documents[*uri];
            occurrences.extend(
                document
                    .definitions
                    .occurrences_targeting(&target_ids)
                    .filter(|(span, target)| {
                        include_declaration
                            || source_file as u32 != target.file
                            || *span != target.span
                    })
                    .map(|(span, _)| (*uri, span)),
            );
        }
        occurrences.sort_unstable_by_key(|(uri, span)| (*uri, span.lo, span.hi));
        occurrences.dedup();
        occurrences
            .into_iter()
            .filter_map(|(uri, span)| {
                let source = self.text_for_uri(uri)?;
                Some(protocol_location(uri, source, span))
            })
            .collect()
    }

    /// References whose targets were bound to a declaration identity. Open documents, closed-file
    /// chunks, and the project symbol index are all searched by that identity. A closed-file use
    /// counts only while the declaration still spells the name recorded with the use.
    fn reference_locations_by_identity(
        &self,
        identities: &HashSet<DeclarationIdentity>,
        include_declaration: bool,
    ) -> Vec<Value> {
        let mut located = Vec::new();
        if include_declaration {
            for identity in identities {
                self.push_declaration_location(&mut located, identity);
            }
        }
        for uri in self.analyzed_uris() {
            let document = &self.documents[uri];
            for (span, target) in document.definitions.occurrences() {
                let Some(identity) = document.definitions.declaration_identity(target) else {
                    continue;
                };
                if !identities.contains(&identity) {
                    continue;
                }
                if !include_declaration && uri == identity.uri && span == identity.span {
                    continue;
                }
                self.push_source_location(&mut located, uri, span);
            }
        }
        for hit in self.workspace_references.matching(identities) {
            if self.documents.contains_key(&hit.uri) {
                continue;
            }
            if !self.retained_declaration_still_matches(&hit) {
                continue;
            }
            if !include_declaration && is_declaration_use(&hit) {
                continue;
            }
            located.push((
                hit.uri,
                [
                    hit.start.line,
                    hit.start.character,
                    hit.end.line,
                    hit.end.character,
                ],
            ));
        }
        located.sort_unstable();
        located.dedup();
        located
            .into_iter()
            .map(|(uri, range)| protocol_range(&uri, range))
            .collect()
    }

    /// The bytes at the recorded span still spell the name captured when the use was indexed.
    ///
    /// An open buffer wins over the symbol index, so an edit is visible before the closed file is
    /// indexed again. A span that now holds a different name is a different declaration.
    fn retained_declaration_still_matches(&self, hit: &WorkspaceReferenceHit) -> bool {
        let Some(current) = self.current_declaration_name(&hit.declaration) else {
            return false;
        };
        let recorded = hit.declaration_name.trim_matches('`');
        let current = current.trim_matches('`');
        !recorded.is_empty() && recorded == current
    }

    fn current_declaration_name(&self, identity: &DeclarationIdentity) -> Option<&str> {
        if let Some(source) = self.text_for_uri(&identity.uri) {
            return source.get(identity.span.lo as usize..identity.span.hi as usize);
        }
        self.project_symbols
            .declaration_name(&identity.uri, identity.span)
    }

    fn push_declaration_location(
        &self,
        located: &mut Vec<(String, [u32; 4])>,
        identity: &DeclarationIdentity,
    ) {
        if self.text_for_uri(&identity.uri).is_some() {
            self.push_source_location(located, &identity.uri, identity.span);
            return;
        }
        if let Some(range) = self
            .project_symbols
            .declaration_range(&identity.uri, identity.span)
        {
            located.push((identity.uri.clone(), range));
        }
    }

    fn push_source_location(&self, located: &mut Vec<(String, [u32; 4])>, uri: &str, span: Span) {
        let Some(source) = self.text_for_uri(uri) else {
            return;
        };
        let start = byte_offset_to_position(source, span.lo as usize);
        let end = byte_offset_to_position(source, span.hi as usize);
        located.push((
            uri.to_string(),
            [start.line, start.character, end.line, end.character],
        ));
    }
}

fn protocol_location(uri: &str, source: &str, span: Span) -> Value {
    json!({
        "uri": uri,
        "range": {
            "start": byte_offset_to_position(source, span.lo as usize),
            "end": byte_offset_to_position(source, span.hi as usize),
        }
    })
}

fn protocol_range(uri: &str, range: [u32; 4]) -> Value {
    json!({
        "uri": uri,
        "range": {
            "start": {"line": range[0], "character": range[1]},
            "end": {"line": range[2], "character": range[3]},
        }
    })
}

fn is_declaration_use(hit: &WorkspaceReferenceHit) -> bool {
    hit.uri == hit.declaration.uri && hit.use_span == hit.declaration.span
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::Duration;

    use serde_json::json;

    use super::super::{AnalysisBackend, LspService, ProjectFeedback};
    use crate::server::engine::{AnalysisBatch, AnalysisJob, IndexBatch, SymbolIndexBatch};
    use crate::IndexedFile;

    struct RecordingBackend {
        ready: bool,
        submitted: Rc<RefCell<Vec<AnalysisJob>>>,
    }

    impl AnalysisBackend for RecordingBackend {
        fn analysis_ready(&self) -> bool {
            self.ready
        }
        fn submit(&mut self, job: AnalysisJob) -> Option<AnalysisBatch> {
            self.submitted.borrow_mut().push(job);
            None
        }
        fn set_workspace_root(&mut self, _root: Option<PathBuf>) -> Option<ProjectFeedback> {
            None
        }
        fn watched_globs(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn note_project_change(&mut self) {}
        fn note_watched_file_change(&mut self, _uri: &str) -> bool {
            false
        }
        fn project_refresh_due_in(&self) -> Option<Duration> {
            None
        }
        fn refresh_project(&mut self) -> Option<ProjectFeedback> {
            None
        }
        fn set_ready(&mut self, ready: bool) {
            self.ready = ready;
        }
    }

    fn service() -> LspService<RecordingBackend> {
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted: Rc::new(RefCell::new(Vec::new())),
        });
        service.force_initialized_for_test();
        service
    }

    fn reference_uris(
        service: &mut LspService<RecordingBackend>,
        uri: &str,
        character: u32,
    ) -> Vec<String> {
        let response = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "references",
            "method": "textDocument/references",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": character},
                "context": {"includeDeclaration": true}
            }
        }));
        response.messages[0]["result"]
            .as_array()
            .expect("reference list")
            .iter()
            .map(|location| location["uri"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn definition_and_references_match_a_closed_file_across_source_sets() {
        let decl_uri = "file:///Decl.kt";
        let use_uri = "file:///Use.kt";
        let other_uri = "file:///Other.kt";
        let declaration = "package demo\nfun answer(): Int = 42\n";
        let use_source = "package demo\nfun use(): Int = answer()\n";
        let other = "package demo\nfun other(): Int = answer()\n";
        let mut service = service();
        service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec![decl_uri.to_string()],
            symbols: crate::WorkspaceSymbolIndex::from_disk_sources(&[(decl_uri, declaration)]),
        });
        service.open_document_for_test(decl_uri, declaration, 1);
        service.open_document_for_test(use_uri, use_source, 1);
        let _ = service.apply_analysis_batch(AnalysisBatch::from_job(
            vec![
                (decl_uri.into(), declaration.into(), 1),
                (use_uri.into(), use_source.into(), 1),
            ],
            crate::analyze_for_lsp(&[declaration, use_source]),
            Vec::new(),
            false,
        ));
        // The source set that numbered the declaration is gone, and the file is closed. The
        // symbol index is what still names it.
        service.source_set.clear();
        service.documents.remove(decl_uri);
        service.analysis_dirty = false;

        let mut closed_uses = crate::analyze_for_lsp(&[declaration, other])[1]
            .definitions
            .clone();
        closed_uses.bind_uris(&[decl_uri, other_uri]);
        closed_uses.record_declaration_names(&[declaration, other]);
        let _ = service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec![other_uri.to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: other_uri.to_string(),
                diagnostics: Vec::new(),
                text_hash: 1,
                references: closed_uses,
                text: other.to_string(),
            }],
        });

        let definition = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "definition",
            "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": use_uri},
                "position": {"line": 1, "character": 18}
            }
        }));
        assert_eq!(
            definition.messages[0]["result"],
            json!([{
                "uri": decl_uri,
                "range": {
                    "start": {"line": 1, "character": 4},
                    "end": {"line": 1, "character": 10}
                }
            }])
        );

        // Same name span the open-file references test pins for `answer`, including the closed
        // file whose source is `fun other(): Int = answer()`.
        let references = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "references",
            "method": "textDocument/references",
            "params": {
                "textDocument": {"uri": use_uri},
                "position": {"line": 1, "character": 18},
                "context": {"includeDeclaration": true}
            }
        }));
        assert_eq!(
            references.messages[0]["result"],
            json!([
                {
                    "uri": decl_uri,
                    "range": {
                        "start": {"line": 1, "character": 4},
                        "end": {"line": 1, "character": 10}
                    }
                },
                {
                    "uri": other_uri,
                    "range": {
                        "start": {"line": 1, "character": 19},
                        "end": {"line": 1, "character": 25}
                    }
                },
                {
                    "uri": use_uri,
                    "range": {
                        "start": {"line": 1, "character": 17},
                        "end": {"line": 1, "character": 23}
                    }
                }
            ])
        );
        let uses_only = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "uses-only",
            "method": "textDocument/references",
            "params": {
                "textDocument": {"uri": use_uri},
                "position": {"line": 1, "character": 18},
                "context": {"includeDeclaration": false}
            }
        }));
        assert_eq!(
            uses_only.messages[0]["result"],
            json!([
                {
                    "uri": other_uri,
                    "range": {
                        "start": {"line": 1, "character": 19},
                        "end": {"line": 1, "character": 25}
                    }
                },
                {
                    "uri": use_uri,
                    "range": {
                        "start": {"line": 1, "character": 17},
                        "end": {"line": 1, "character": 23}
                    }
                }
            ])
        );
    }

    #[test]
    fn a_closed_caller_does_not_follow_an_equal_length_rename() {
        let decl_uri = "file:///Decl.kt";
        let use_uri = "file:///Use.kt";
        let original = "package demo\nfun aa(): Int = 1\nfun bb(): Int = 2\n";
        let swapped = "package demo\nfun bb(): Int = 1\nfun aa(): Int = 2\n";
        let caller = "package demo\nfun use(): Int = aa()\n";
        let mut service = service();
        service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec![decl_uri.to_string()],
            symbols: crate::WorkspaceSymbolIndex::from_disk_sources(&[(decl_uri, original)]),
        });
        let mut closed_uses = crate::analyze_for_lsp(&[original, caller])[1]
            .definitions
            .clone();
        closed_uses.bind_uris(&[decl_uri, use_uri]);
        closed_uses.record_declaration_names(&[original, caller]);
        let _ = service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec![use_uri.to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: use_uri.to_string(),
                diagnostics: Vec::new(),
                text_hash: 1,
                references: closed_uses,
                text: caller.to_string(),
            }],
        });

        service.open_document_for_test(decl_uri, original, 1);
        let _ = service.apply_analysis_batch(AnalysisBatch::from_job(
            vec![(decl_uri.into(), original.into(), 1)],
            crate::analyze_for_lsp(&[original]),
            Vec::new(),
            false,
        ));
        service.analysis_dirty = false;
        let before = reference_uris(&mut service, decl_uri, 4);
        assert!(
            before.contains(&use_uri.to_string()),
            "the closed caller of aa is a reference before the rename: {before:?}"
        );

        service.open_document_for_test(decl_uri, swapped, 2);
        let _ = service.apply_analysis_batch(AnalysisBatch::from_job(
            vec![(decl_uri.into(), swapped.into(), 2)],
            crate::analyze_for_lsp(&[swapped]),
            Vec::new(),
            false,
        ));
        service.analysis_dirty = false;
        // `bb` now occupies the bytes that named `aa`. The closed call still says `aa`.
        let after = reference_uris(&mut service, decl_uri, 4);
        assert!(
            !after.contains(&use_uri.to_string()),
            "swapping equal-length names must not retarget the closed caller onto bb: {after:?}"
        );
        assert!(
            after.contains(&decl_uri.to_string()),
            "the renamed declaration is still a reference of itself: {after:?}"
        );
    }
}
