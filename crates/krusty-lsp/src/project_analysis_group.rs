//! One project's analysis-group identity, configuration, fingerprint, and cached result lifecycle.

use super::analysis_group::ProjectAnalysisGroup;
use krusty_lsp::{DocumentAnalysis, LspOptions, ProjectModel};
use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;

pub(super) struct CachedProjectAnalysis {
    pub(super) module_index: Option<usize>,
    pub(super) fingerprint: u64,
    pub(super) document_indices: Vec<usize>,
    pub(super) analyses: Vec<DocumentAnalysis>,
    pub(super) retained_bytes: usize,
}

/// Partition the open documents into the source sets that are analyzed together.
///
/// Each module owns one group, so a module's analysis never sees another module's sources. A
/// document the model claims for no module — a scratch file, a source root the build system does
/// not describe — gets a group to ITSELF, carrying no module classpath and no support sources.
/// Pooling the unowned documents into one group instead would let two unrelated scratch files see
/// each other's top-level declarations, so the same `fun` in each would report a conflict; dropping
/// them would leave them open in the editor with no diagnostics, no completion, and no navigation.
///
/// A workspace with no model at all is a different case and does not reach here: every document is
/// assigned the one synthetic module by `project_module_assignments`, because a plain folder of
/// `.kt` files is meant to be one source set.
pub(super) fn project_analysis_groups(
    module_assignments: &[Option<usize>],
) -> Vec<(Option<usize>, Vec<usize>)> {
    let mut groups: Vec<(Option<usize>, Vec<usize>)> = Vec::new();
    for (document_index, module_index) in module_assignments.iter().copied().enumerate() {
        match module_index.and_then(|module| {
            groups
                .iter_mut()
                .find(|(candidate, _)| *candidate == Some(module))
        }) {
            Some((_, document_indices)) => document_indices.push(document_index),
            None => groups.push((module_index, vec![document_index])),
        }
    }
    groups
}

pub(super) fn project_group_compiler_config(
    model: Option<&ProjectModel>,
    module_index: Option<usize>,
    platform_classpath: &[PathBuf],
    options: &LspOptions,
) -> (Option<Vec<PathBuf>>, Vec<String>) {
    let Some((model, module)) = model
        .zip(module_index)
        .and_then(|(model, index)| model.modules.get(index).map(|module| (model, module)))
    else {
        return (None, options.language_arguments().to_vec());
    };

    let mut classpath = model.compile_classpath(module);
    for entry in platform_classpath {
        if !classpath.contains(entry) {
            classpath.push(entry.clone());
        }
    }
    let mut language_arguments = module.kotlinc_args.clone();
    language_arguments.extend_from_slice(options.language_arguments());
    (Some(classpath), language_arguments)
}

pub(super) fn open_documents_from_modules<'a>(
    visible_indices: &[usize],
    documents: &[(&'a str, &'a str)],
    module_assignments: &[Option<usize>],
) -> Vec<(usize, &'a str, &'a str)> {
    let visible: HashSet<usize> = visible_indices.iter().copied().collect();
    documents
        .iter()
        .zip(module_assignments)
        .enumerate()
        .filter_map(|(document_index, ((uri, source), assignment))| {
            if assignment.is_some_and(|index| visible.contains(&index)) {
                Some((document_index, *uri, *source))
            } else {
                None
            }
        })
        .collect()
}

pub(super) fn project_group_fingerprint(
    documents: &[(&str, &str)],
    group: &ProjectAnalysisGroup<'_>,
) -> u64 {
    let mut fingerprint = DefaultHasher::new();
    documents.len().hash(&mut fingerprint);
    group.module_index.hash(&mut fingerprint);
    group.document_indices.hash(&mut fingerprint);
    group.inferred_support_count.hash(&mut fingerprint);
    group.navigation_file_remaps.hash(&mut fingerprint);
    for (index, (uri, source)) in documents.iter().enumerate() {
        if group.contains_document(index) {
            uri.hash(&mut fingerprint);
            krusty_lsp::open_document_digest::text_hash(uri, source).hash(&mut fingerprint);
        }
    }
    for source in &group.support_documents {
        source.digest().hash(&mut fingerprint);
    }
    for source in &group.java_sources {
        source.digest().hash(&mut fingerprint);
    }
    fingerprint.finish()
}

pub(super) fn retain_analysis_cache_budget(
    cache: &mut Vec<CachedProjectAnalysis>,
    incoming_bytes: usize,
    max_bytes: usize,
) {
    let mut retained = cache
        .iter()
        .map(|cached| cached.retained_bytes)
        .sum::<usize>();
    while !cache.is_empty() && incoming_bytes > max_bytes.saturating_sub(retained) {
        retained = retained.saturating_sub(cache.remove(0).retained_bytes);
    }
}

pub(super) fn source_bytes<'a>(sources: impl IntoIterator<Item = &'a str>) -> Option<usize> {
    sources
        .into_iter()
        .try_fold(0usize, |bytes, source| bytes.checked_add(source.len()))
}

pub(super) fn project_source_size_limit_message() -> String {
    format!(
        "module source set exceeds analysis limit (maximum {} MiB); semantic diagnostics suppressed",
        krusty_lsp::MAX_SOURCE_SET_BYTES / (1024 * 1024)
    )
}

fn project_source_error_analysis(message: &str) -> DocumentAnalysis {
    DocumentAnalysis::with_diagnostics(vec![krusty::diag::Diagnostic {
        span: krusty::diag::Span::new(0, 0),
        editor_span: None,
        identity: None,
        severity: krusty::diag::Severity::Error,
        kind: krusty::diag::DiagnosticKind::Compiler,
        msg: message.to_string(),
        file: 0,
    }])
}

pub(super) fn fail_project_group(
    analyses: &mut [DocumentAnalysis],
    cache: &mut Vec<CachedProjectAnalysis>,
    module_index: Option<usize>,
    document_indices: &[usize],
    message: &str,
) {
    cache.retain(|cached| cached.module_index != module_index);
    for &index in document_indices {
        analyses[index] = project_source_error_analysis(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty_lsp::{AnalysisBackend, DocumentAnalysis};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn fingerprint_uses_only_group_members_in_source_order() {
        let documents = [
            ("file:///a.kt", "fun a() {}"),
            ("file:///b.kt", "fun b() {}"),
            ("file:///c.kt", "fun c() {}"),
        ];
        let group =
            ProjectAnalysisGroup::new(Some(1), vec![2, 0], Vec::new(), 0, Vec::new(), Vec::new());
        let fingerprint = project_group_fingerprint(&documents, &group);

        let outsider_changed = [
            documents[0],
            ("file:///b.kt", "fun changed() {}"),
            documents[2],
        ];
        assert_eq!(
            project_group_fingerprint(&outsider_changed, &group),
            fingerprint
        );

        let member_changed = [
            ("file:///a.kt", "fun changed() {}"),
            documents[1],
            documents[2],
        ];
        assert_ne!(
            project_group_fingerprint(&member_changed, &group),
            fingerprint
        );

        let source_order =
            ProjectAnalysisGroup::new(Some(1), vec![0, 2], Vec::new(), 0, Vec::new(), Vec::new());
        assert_ne!(
            project_group_fingerprint(&documents, &source_order),
            fingerprint
        );
    }

    struct FingerprintAnalysis {
        fingerprints: Rc<RefCell<Vec<u64>>>,
    }

    impl krusty_lsp::Analysis for FingerprintAnalysis {
        fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
            sources.iter().map(|_| DocumentAnalysis::empty()).collect()
        }

        fn index_workspace_files(&mut self, _uris: &[&str]) -> krusty_lsp::IndexOutcome {
            krusty_lsp::IndexOutcome::default()
        }

        fn analyze_open_documents(
            &mut self,
            documents: &[(&str, &str)],
            _open_uris: &[&str],
        ) -> (Vec<DocumentAnalysis>, Vec<(String, String)>) {
            let group = ProjectAnalysisGroup::new(
                None,
                (0..documents.len()).collect(),
                Vec::new(),
                0,
                Vec::new(),
                Vec::new(),
            );
            self.fingerprints
                .borrow_mut()
                .push(project_group_fingerprint(documents, &group));
            let sources = documents
                .iter()
                .map(|(_, source)| *source)
                .collect::<Vec<_>>();
            (self.analyze(&sources), Vec::new())
        }
    }

    #[test]
    fn inline_backend_fingerprint_changes_for_a_reopened_document() {
        let text = "x".repeat(64 * 1024);
        let reopened = "y".repeat(text.len());
        let job = |body: &str, lifetime: u64| krusty_lsp::AnalysisJob {
            documents: vec![("file:///a.kt".into(), body.to_string(), 1, lifetime)],
            open_uris: vec!["file:///a.kt".into()],
        };
        let fingerprints = Rc::new(RefCell::new(Vec::new()));
        let mut backend = krusty_lsp::InlineBackend::new(FingerprintAnalysis {
            fingerprints: fingerprints.clone(),
        });
        let first_lifetime = krusty_lsp::open_document_digest::next_document_lifetime();
        let reopened_lifetime = krusty_lsp::open_document_digest::next_document_lifetime();
        assert!(backend.submit(job(&text, first_lifetime)).is_some());
        assert!(backend.submit(job(&text, first_lifetime)).is_some());
        assert!(backend.submit(job(&reopened, reopened_lifetime)).is_some());

        let fingerprints = fingerprints.borrow();
        assert_eq!(fingerprints.len(), 3);
        assert_eq!(fingerprints[0], fingerprints[1]);
        assert_ne!(fingerprints[1], fingerprints[2]);
    }
}
