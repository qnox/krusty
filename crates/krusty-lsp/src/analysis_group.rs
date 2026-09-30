//! One module's open documents for a single analysis pass.
//!
//! `document_indices` is the source order of those documents. Membership is a
//! set built with the group, so a later producer can hand the same indices in
//! any order without blanking an in-group file or dropping it from the cache key.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

pub(super) struct ProjectAnalysisGroup<'a> {
    pub(super) module_index: Option<usize>,
    pub(super) document_indices: Vec<usize>,
    members: HashSet<usize>,
    pub(super) support_documents: Vec<(&'a str, &'a str)>,
    pub(super) inferred_support_count: usize,
    pub(super) java_sources: Vec<String>,
    pub(super) navigation_file_remaps: Vec<(u32, u32)>,
}

impl<'a> ProjectAnalysisGroup<'a> {
    pub(super) fn new(
        module_index: Option<usize>,
        document_indices: Vec<usize>,
        support_documents: Vec<(&'a str, &'a str)>,
        inferred_support_count: usize,
        java_sources: Vec<String>,
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

    fn contains_document(&self, index: usize) -> bool {
        self.members.contains(&index)
    }
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
                    super::source_kind_from_uri(uri),
                    if in_group { source } else { "" },
                ),
            )
        })
        .chain(group.support_documents.iter().map(|(uri, source)| {
            (
                "",
                krusty::source::SourceInput::new(super::source_kind_from_uri(uri), source),
            )
        }))
}

pub(super) fn project_group_inputs<'a>(
    documents: &'a [(&'a str, &'a str)],
    group: &'a ProjectAnalysisGroup<'a>,
) -> Vec<krusty::source::SourceInput<'a>> {
    project_group_slots(documents, group)
        .map(|(_, input)| input)
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
            source.hash(&mut fingerprint);
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
}
