//! One module's open documents for a single analysis pass.
//!
//! `document_indices` is the source order of those documents. Membership is a
//! set built with the group, so a later producer can hand the same indices in
//! any order without blanking an in-group file or dropping it from the cache key.

use std::collections::HashSet;

pub(super) struct ProjectAnalysisGroup<'a> {
    pub(super) module_index: Option<usize>,
    pub(super) document_indices: Vec<usize>,
    members: HashSet<usize>,
    pub(super) support_documents: Vec<krusty_lsp::SupportText<'a>>,
    pub(super) inferred_support_count: usize,
    pub(super) java_sources: Vec<&'a krusty_lsp::DigestedJavaSource>,
    pub(super) navigation_file_remaps: Vec<(u32, u32)>,
}

impl<'a> ProjectAnalysisGroup<'a> {
    pub(super) fn new(
        module_index: Option<usize>,
        document_indices: Vec<usize>,
        support_documents: Vec<krusty_lsp::SupportText<'a>>,
        inferred_support_count: usize,
        java_sources: Vec<&'a krusty_lsp::DigestedJavaSource>,
        navigation_file_remaps: Vec<(u32, u32)>,
    ) -> Self {
        let members = document_indices.iter().copied().collect();
        Self {
            module_index,
            document_indices,
            members,
            support_documents,
            inferred_support_count,
            java_sources,
            navigation_file_remaps,
        }
    }

    pub(super) fn contains_document(&self, index: usize) -> bool {
        self.members.contains(&index)
    }
}

fn source_kind_from_uri(uri: &str) -> krusty::source::SourceKind {
    if uri.ends_with(".java") {
        return krusty::source::SourceKind::Java;
    }
    url::Url::parse(uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok())
        .as_deref()
        .and_then(krusty::source::kind)
        .unwrap_or(krusty::source::SourceKind::Kotlin)
}

/// A script document's file stem, which names its class: the decoded file name without its
/// extension. Any other document has none.
fn script_stem(uri: &str) -> Option<String> {
    if source_kind_from_uri(uri) != krusty::source::SourceKind::KotlinScript {
        return None;
    }
    let path = url::Url::parse(uri).ok()?.to_file_path().ok()?;
    Some(path.file_stem()?.to_str()?.to_string())
}

/// The group's worker slots in wire order, each paired with the URI it may be dumped under.
///
/// One traversal so the source inputs and the dump URIs cannot drift apart: a slot's URI is empty
/// exactly when that slot is not a primary document of this group, and a dump of an empty URI is
/// never offered. Two kinds of slot are deliberately blank:
///
/// - open documents belonging to another group, whose text this group blanks out anyway; and
/// - the support tail, which carries friend and dependency sources — including *open* files from
///   another module. Those are analyzed here under this group's classpath and language arguments,
///   not their own module's, so dumping one would render unresolved types and feature errors for a
///   file the editor shows as clean.
///
/// The URI is a borrowed `&str` rather than an owned `String` because this traversal is on the
/// analysis hot path; only the dev-mode dump path pays for copies.
pub(super) fn project_group_slots<'a>(
    documents: &'a [(&'a str, &'a str)],
    group: &'a ProjectAnalysisGroup<'a>,
) -> impl Iterator<Item = (&'a str, krusty::source::SourceInput<'a>)> + 'a {
    documents
        .iter()
        .enumerate()
        .map(move |(index, (uri, source))| {
            let in_group = group.contains_document(index);
            (
                if in_group { *uri } else { "" },
                krusty::source::SourceInput::new(
                    source_kind_from_uri(uri),
                    if in_group { source } else { "" },
                ),
            )
        })
        .chain(group.support_documents.iter().map(|source| {
            (
                "",
                krusty::source::SourceInput::new(source_kind_from_uri(source.uri()), source.text()),
            )
        }))
}

/// Each slot's script file stem, parallel to `project_group_inputs`. The inputs borrow them, so a
/// caller keeps these alive while it uses the inputs.
pub(super) fn project_group_script_stems(
    documents: &[(&str, &str)],
    group: &ProjectAnalysisGroup<'_>,
) -> Vec<Option<String>> {
    documents
        .iter()
        .map(|(uri, _)| script_stem(uri))
        .chain(
            group
                .support_documents
                .iter()
                .map(|source| script_stem(source.uri())),
        )
        .collect()
}

pub(super) fn project_group_inputs<'a>(
    documents: &'a [(&'a str, &'a str)],
    group: &'a ProjectAnalysisGroup<'a>,
    script_stems: &'a [Option<String>],
) -> Vec<krusty::source::SourceInput<'a>> {
    project_group_slots(documents, group)
        .zip(script_stems)
        .map(|((_, input), stem)| match stem {
            Some(stem) => input.with_file_stem(stem),
            None => input,
        })
        .collect()
}

/// Dump URIs parallel to `project_group_inputs`, blank wherever the slot is not dumpable.
pub(super) fn project_group_uris<'a>(
    documents: &'a [(&'a str, &'a str)],
    group: &'a ProjectAnalysisGroup<'a>,
) -> Vec<String> {
    project_group_slots(documents, group)
        .map(|(uri, _)| uri.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsorted_indices_stay_members_and_keep_their_order() {
        let documents = [
            ("file:///a.kt", "fun a() {}"),
            ("file:///b.kt", "fun b() {}"),
            ("file:///c.kt", "fun c() {}"),
        ];
        let group =
            ProjectAnalysisGroup::new(Some(1), vec![2, 0], Vec::new(), 0, Vec::new(), Vec::new());
        assert_eq!(group.document_indices, [2, 0]);

        let slots = project_group_slots(&documents, &group)
            .take(documents.len())
            .map(|(uri, input)| (uri, input.text))
            .collect::<Vec<_>>();
        assert_eq!(
            slots,
            [
                ("file:///a.kt", "fun a() {}"),
                ("", ""),
                ("file:///c.kt", "fun c() {}"),
            ]
        );
    }

    #[test]
    fn a_script_is_named_after_its_decoded_file_stem() {
        let documents = [
            ("file:///work/my%20script.kts", "val a = 1"),
            ("file:///work/build.gradle.kts", "val b = 2"),
            ("file:///work/Main.kt", "fun main() {}"),
        ];
        let group = ProjectAnalysisGroup::new(
            Some(0),
            vec![0, 1, 2],
            Vec::new(),
            0,
            Vec::new(),
            Vec::new(),
        );
        let stems = project_group_script_stems(&documents, &group);
        let inputs = project_group_inputs(&documents, &group, &stems)
            .iter()
            .map(|input| input.file_stem)
            .collect::<Vec<_>>();

        assert_eq!(inputs, [Some("my script"), Some("build.gradle"), None]);
    }
}
