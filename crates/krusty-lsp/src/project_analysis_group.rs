//! One project's analysis group identity and its cache fingerprint.

use super::analysis_group::ProjectAnalysisGroup;
use krusty_lsp::{LspOptions, ProjectModel};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;

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
    for (uri, source) in &group.support_documents {
        uri.hash(&mut fingerprint);
        source.hash(&mut fingerprint);
    }
    group.java_sources.hash(&mut fingerprint);
    fingerprint.finish()
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
        ) -> (
            Vec<DocumentAnalysis>,
            Vec<(String, krusty_lsp::SharedSource)>,
        ) {
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
