//! Splice order for one analysis group's support tail.
//!
//! Disk support stays borrowed from the source cache. Friend and dependency
//! files stay borrowed from the open buffers. The cache lookup hashes and
//! budgets those borrows; it does not own a second copy of the text.

/// Support order is the inferred disk prefix, then open friends, then open
/// dependencies, then the remaining disk files.
///
/// Friends count as inferred support. Open dependency documents do not: they
/// are visible to the group, but they are not part of the inferred prefix the
/// worker treats as generated sources.
pub(super) fn spliced_support<'a>(
    disk: &'a [krusty_lsp::DigestedSource],
    inferred_count: usize,
    friends: &[(usize, &'a str, &'a str)],
    dependencies: &[(usize, &'a str, &'a str)],
) -> (Vec<krusty_lsp::SupportText<'a>>, usize) {
    let (head, tail) = disk.split_at(inferred_count);
    let mut pairs = Vec::with_capacity(disk.len() + friends.len() + dependencies.len());
    pairs.extend(head.iter().map(krusty_lsp::DigestedSource::as_support));
    pairs.extend(
        friends
            .iter()
            .map(|(_, uri, source)| krusty_lsp::SupportText::open_document(uri, source)),
    );
    let inferred_support_count = pairs.len();
    pairs.extend(
        dependencies
            .iter()
            .map(|(_, uri, source)| krusty_lsp::SupportText::open_document(uri, source)),
    );
    pairs.extend(tail.iter().map(krusty_lsp::DigestedSource::as_support));
    (pairs, inferred_support_count)
}

#[cfg(test)]
mod tests {
    use super::spliced_support;

    #[test]
    fn friends_stay_in_the_inferred_prefix_and_dependencies_follow_it() {
        let disk = [
            krusty_lsp::DigestedSource::kotlin("file:///inferred.kt", "fun inferred() {}"),
            krusty_lsp::DigestedSource::kotlin("file:///rest.kt", "fun rest() {}"),
        ];
        let friends = [(1, "file:///friend.kt", "fun friend() {}")];
        let dependencies = [(2, "file:///dep.kt", "fun dep() {}")];

        let (pairs, inferred) = spliced_support(&disk, 1, &friends, &dependencies);

        assert_eq!(inferred, 2, "an open friend extends the inferred prefix");
        assert_eq!(
            pairs
                .iter()
                .map(|source| (source.uri(), source.text(), source.digest()))
                .collect::<Vec<_>>(),
            [
                disk[0].as_support(),
                krusty_lsp::SupportText::open_document("file:///friend.kt", "fun friend() {}",),
                krusty_lsp::SupportText::open_document("file:///dep.kt", "fun dep() {}"),
                disk[1].as_support(),
            ]
            .into_iter()
            .map(|source| (source.uri(), source.text(), source.digest()))
            .collect::<Vec<_>>()
        );

        let (pairs, inferred) = spliced_support(&disk, 1, &[], &[]);
        assert_eq!(
            inferred, 1,
            "no open friend leaves the disk prefix unchanged"
        );
        assert_eq!(
            pairs
                .iter()
                .map(|source| source.digest())
                .collect::<Vec<_>>(),
            vec![disk[0].digest(), disk[1].digest()]
        );
    }

    #[test]
    fn the_analysis_pass_does_not_copy_support_before_the_cache_lookup() {
        let source = include_str!("main.rs");
        let pass = source
            .split_once("fn analyze_open_documents(")
            .expect("analysis pass")
            .1;
        let pass = pass.split_once("\n    fn ").expect("next method").0;
        assert!(
            !pass.contains("sources.to_vec()"),
            "copying disk support before the cache lookup duplicates every support file on a hit"
        );
        assert!(
            !pass.contains("source.clone()"),
            "friend and dependency buffers must stay borrowed through the cache lookup"
        );
    }
}
