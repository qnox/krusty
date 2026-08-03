//! Searchable class names from the project's dependencies.
//!
//! The project index answers for the workspace's own sources. This answers for what they depend
//! on — the union of every module's compile classpath, which is where most of the names a reader
//! looks up actually live. Only that: not every jar on the machine, and not the build cache it was
//! resolved from. What the project does not compile against, nobody in the project can name.
//!
//! It holds names only. A dependency symbol has no location until its class is rendered back to
//! source, and rendering is on demand — for the symbols a query actually returns, never ahead of
//! time. Ranking therefore happens here, over names, and produces candidates; turning a candidate
//! into something a client can open is a separate step, taken for as few as possible.
//!
//! Building it costs about **6 µs per class**, measured across disjoint slices of a real Gradle
//! cache at 4k, 28k, 55k and 79k classes — steady enough that class count, not jar count, is what
//! predicts the cost. A dependency set of 200,000 classes is therefore a little over a second, once
//! per project model. Jar count predicts poorly: a 200-jar slice held fewer classes than a 100-jar
//! one.

use std::collections::HashMap;

use crate::analysis::{
    camel_hump_initials, is_ordered_subsequence_lowercase, matches_glob, workspace_queries,
    WorkspaceQuery, WorkspaceSymbolRung, MAX_WORKSPACE_SYMBOL_GLOB_STEPS,
};

/// Ceiling on raw class entries inspected and searchable classes retained for one project.
///
/// Measured against real dependency sets rather than guessed: 100 jars declare 91,182 classes,
/// 200 declare 219,272, and 400 declare 392,275. A 256k ceiling truncated the 400-jar set exactly,
/// which is a large but ordinary enterprise dependency graph, so it sits above that.
///
/// Applying it before filtering also bounds malformed or synthetic-heavy archives whose entries
/// would never become searchable. The retained cost is deliberately two `u32`s per accepted class
/// plus interned names — the slashed internal name is *derived* rather than stored, because one per
/// class is the largest table there would be and it is recoverable from the package and the name.
pub const MAX_DEPENDENCY_CLASSES: usize = 1024 * 1024;

/// A class the classpath declares, ranked but not yet located.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyCandidate {
    /// Slashed internal name, as the classpath spells it: `kotlin/collections/AbstractList`.
    pub internal: String,
    /// Dotted package, empty for the default package.
    pub package: String,
    /// Simple name, with nested classes kept as `Outer.Inner`.
    pub name: String,
}

/// Class names from one classpath, ordered for the same rung ladder the source index uses.
#[derive(Default)]
pub struct DependencySymbolIndex {
    /// `(name id, package id)`, one per class.
    entries: Vec<[u32; 2]>,
    names: Vec<String>,
    lowercase_names: Vec<String>,
    initials: Vec<String>,
    packages: Vec<String>,
    by_name: Vec<u32>,
    /// Nested-class entries sorted by their final source-name segment (`Entry` for `Map.Entry`).
    /// Only nested entries participate, avoiding a full classpath scan for ordinary queries.
    by_simple_segment: Vec<u32>,
    by_initials: Vec<u32>,
    complete: bool,
}

impl DependencySymbolIndex {
    /// Build from the project's resolved classpath: the union of every module's compile classpath.
    ///
    /// Scope is deliberate. A build cache holds every version of every artifact ever fetched, and
    /// none of what the project does not compile against can be named from it, so indexing the
    /// cache would answer with classes no source file could refer to.
    pub fn from_classpath(classpath: &krusty::jvm::classpath::Classpath) -> Self {
        Self::from_internal_names(
            classpath
                .package_tree()
                .classes()
                .map(|(internal, _)| internal),
        )
    }

    /// Build from the project's classpath, reading each jar's class list from the cache when it is
    /// there and filling it when it is not.
    ///
    /// Jar by jar rather than all at once, because that is the granularity the cache shares at: two
    /// projects rarely resolve the same *set* of artifacts, and constantly resolve the same
    /// individual ones. Either way the listings have to be deduplicated -- a classpath can carry two
    /// versions of the same artifact, and a name both declare is one class, resolved from whichever
    /// entry comes first.
    pub fn from_cached_classpath(
        entries: &[std::path::PathBuf],
        cache_root: &std::path::Path,
    ) -> Self {
        // Keep the per-entry vectors needed for cache IO, but feed them into the bounded builder
        // lazily. Extending one classpath-wide `Vec` first made the index's retention ceiling take
        // effect only after every raw name had already been retained.
        let names = entries.iter().flat_map(|entry| {
            let classes = match crate::dependency_cache::load(cache_root, entry) {
                Some(cached) => cached,
                None => {
                    let classes = jar_class_names(entry);
                    crate::dependency_cache::store(cache_root, entry, &classes);
                    classes
                }
            };
            classes.into_iter()
        });
        Self::from_internal_names(names)
    }

    /// Build from slashed internal names, as `Classpath::package_tree().classes()` yields them.
    ///
    /// A `$` in an internal name is a nested class, and a reader searching for `Entry` means
    /// `Map.Entry`, so the separator becomes `.` and the simple name keeps its outer classes.
    /// Synthetic names — anonymous classes, lambda carriers — are dropped: nobody searches for
    /// `Foo$1`, and they outnumber the real declarations in some jars.
    ///
    /// Repeats collapse. A classpath can carry two versions of one artifact, and a name both
    /// declare is a single class, resolved from whichever entry comes first.
    pub fn from_internal_names(internals: impl IntoIterator<Item = String>) -> Self {
        Self::from_internal_names_with_limit(internals, MAX_DEPENDENCY_CLASSES)
    }

    fn from_internal_names_with_limit(
        internals: impl IntoIterator<Item = String>,
        limit: usize,
    ) -> Self {
        let mut result = Self {
            complete: true,
            ..Self::default()
        };
        let mut name_ids = HashMap::<String, u32>::new();
        let mut package_ids = HashMap::<String, u32>::new();
        let mut declared = std::collections::HashSet::new();
        for (inspected, internal) in internals.into_iter().enumerate() {
            // The work/temporary-retention ceiling applies before filtering. A hostile archive can
            // contain an arbitrary number of multi-release, descriptor, or synthetic entries that
            // never become searchable; counting only accepted classes left that input unbounded.
            if inspected >= limit {
                result.complete = false;
                break;
            }
            if !declared.insert(internal.clone()) {
                continue;
            }
            let Some((package, simple)) = split_internal_name(&internal) else {
                continue;
            };
            let name_id = intern(&simple, &mut result.names, &mut name_ids);
            let package_id = intern(&package, &mut result.packages, &mut package_ids);
            result.entries.push([name_id, package_id]);
        }
        result.rebuild_search_order();
        result
    }

    fn rebuild_search_order(&mut self) {
        self.lowercase_names = self
            .names
            .iter()
            .map(|name| name.to_lowercase())
            .collect::<Vec<_>>();
        self.initials = self
            .names
            .iter()
            .map(|name| camel_hump_initials(name))
            .collect::<Vec<_>>();
        let mut by_name = (0..self.entries.len() as u32).collect::<Vec<u32>>();
        by_name.sort_unstable_by(|left, right| {
            self.name_of(*left, &self.lowercase_names)
                .cmp(self.name_of(*right, &self.lowercase_names))
                .then_with(|| left.cmp(right))
        });
        let mut by_initials = (0..self.entries.len() as u32).collect::<Vec<u32>>();
        by_initials.sort_unstable_by(|left, right| {
            self.name_of(*left, &self.initials)
                .cmp(self.name_of(*right, &self.initials))
                .then_with(|| left.cmp(right))
        });
        let mut by_simple_segment = (0..self.entries.len() as u32)
            .filter(|index| self.name_of(*index, &self.lowercase_names).contains('.'))
            .collect::<Vec<_>>();
        by_simple_segment.sort_unstable_by(|left, right| {
            self.simple_segment(*left)
                .cmp(self.simple_segment(*right))
                .then_with(|| left.cmp(right))
        });
        self.by_name = by_name;
        self.by_simple_segment = by_simple_segment;
        self.by_initials = by_initials;
    }

    pub fn class_count(&self) -> usize {
        self.entries.len()
    }

    /// Whether every class offered was indexed.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// The best `limit` classes for `query`, strongest rung first.
    ///
    /// The ladder is the source index's: name prefix, then camel-hump initials, then a subsequence
    /// over the initials, then over the name. Ranking is all that happens here — a candidate is a
    /// name, and turning it into something a client can open is the caller's job, for only as many
    /// as it means to return.
    pub fn candidates(&self, query: &str, limit: usize) -> Vec<DependencyCandidate> {
        let mut remaining_glob_steps = MAX_WORKSPACE_SYMBOL_GLOB_STEPS;
        self.candidates_with_glob_steps(query, limit, &mut remaining_glob_steps)
    }

    /// Rank with the wildcard transition budget left by earlier workspace-symbol layers.
    pub(crate) fn candidates_with_glob_steps(
        &self,
        query: &str,
        limit: usize,
        remaining_glob_steps: &mut usize,
    ) -> Vec<DependencyCandidate> {
        if limit == 0 {
            return Vec::new();
        }
        let mut ranked = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for query in workspace_queries(query) {
            // The source layer may use an empty query to enumerate a bounded project. Dependencies
            // are the widest layer and deliberately decline it rather than return an arbitrary
            // slice of the classpath. Every other query semantic is shared.
            if query.is_empty() || ranked.len() >= limit || *remaining_glob_steps == 0 {
                continue;
            }
            let indices = self.ranked_indices(
                &query,
                limit - ranked.len(),
                &mut seen,
                remaining_glob_steps,
            );
            for index in indices {
                if let Some(candidate) = self.candidate(index) {
                    ranked.push(candidate);
                }
            }
        }
        ranked
    }

    /// Rank at most `limit` previously unseen entries for one parsed query.
    ///
    /// The limit is enforced while each rung is walked, not after a vector has been built. A one-
    /// character prefix can match hundreds of thousands of classes; materialising all of them for
    /// a 32-result response would make the response cap cosmetic rather than a work bound.
    fn ranked_indices(
        &self,
        query: &WorkspaceQuery,
        limit: usize,
        seen: &mut std::collections::HashSet<u32>,
        remaining_glob_steps: &mut usize,
    ) -> Vec<u32> {
        let mut ranked = Vec::new();
        if limit == 0 {
            return ranked;
        }
        for rung in query.rungs() {
            if ranked.len() >= limit {
                break;
            }
            match rung {
                WorkspaceSymbolRung::Every => {}
                WorkspaceSymbolRung::Glob => {
                    let prefix = query.literal_prefix();
                    let completed = if prefix.is_empty() {
                        self.append_glob_candidates(
                            0..self.entries.len() as u32,
                            query,
                            limit,
                            seen,
                            &mut ranked,
                            remaining_glob_steps,
                        )
                    } else {
                        self.append_glob_candidates(
                            self.prefix_range(&self.by_name, &self.lowercase_names, prefix)
                                .iter()
                                .copied(),
                            query,
                            limit,
                            seen,
                            &mut ranked,
                            remaining_glob_steps,
                        )
                    };
                    if !completed {
                        break;
                    }
                }
                WorkspaceSymbolRung::NamePrefix => {
                    for &index in
                        self.prefix_range(&self.by_name, &self.lowercase_names, &query.pattern)
                    {
                        if !self.admit_ranked(index, query, limit, seen, &mut ranked) {
                            break;
                        }
                    }
                    if ranked.len() < limit {
                        // Nested declarations are source-visible by their leaf name with or without
                        // an outer qualifier: `Entry` and `Map.Entry` both mean the entry represented
                        // internally as `Map$Entry`. A dedicated sorted subset keeps that semantic
                        // lookup logarithmic instead of scanning every class after most prefixes.
                        for &index in self.simple_segment_prefix_range(&query.pattern) {
                            if !self
                                .name_of(index, &self.lowercase_names)
                                .starts_with(&query.pattern)
                                && !self.admit_ranked(index, query, limit, seen, &mut ranked)
                            {
                                break;
                            }
                        }
                    }
                }
                WorkspaceSymbolRung::InitialsPrefix => {
                    for &index in
                        self.prefix_range(&self.by_initials, &self.initials, &query.pattern)
                    {
                        if self
                            .name_of(index, &self.lowercase_names)
                            .starts_with(&query.pattern)
                        {
                            continue;
                        }
                        if !self.admit_ranked(index, query, limit, seen, &mut ranked) {
                            break;
                        }
                    }
                }
                WorkspaceSymbolRung::InitialsSubsequence => {
                    let matching = self.matching_names(|name, initials| {
                        is_ordered_subsequence_lowercase(initials, &query.pattern)
                            && !initials.starts_with(&query.pattern)
                            && !name.starts_with(&query.pattern)
                    });
                    for &index in &self.by_initials {
                        if self.name_matches(index, &matching)
                            && !self.admit_ranked(index, query, limit, seen, &mut ranked)
                        {
                            break;
                        }
                    }
                }
                WorkspaceSymbolRung::NameSubsequence => {
                    let matching = self.matching_names(|name, initials| {
                        is_ordered_subsequence_lowercase(name, &query.pattern)
                            && !name.starts_with(&query.pattern)
                            && !is_ordered_subsequence_lowercase(initials, &query.pattern)
                    });
                    for index in 0..self.entries.len() as u32 {
                        if self.name_matches(index, &matching)
                            && !self.admit_ranked(index, query, limit, seen, &mut ranked)
                        {
                            break;
                        }
                    }
                }
            }
        }
        ranked
    }

    fn append_glob_candidates(
        &self,
        candidates: impl IntoIterator<Item = u32>,
        query: &WorkspaceQuery,
        limit: usize,
        seen: &mut std::collections::HashSet<u32>,
        ranked: &mut Vec<u32>,
        remaining_steps: &mut usize,
    ) -> bool {
        for index in candidates {
            match matches_glob(
                self.name_of(index, &self.lowercase_names),
                &query.pattern,
                remaining_steps,
            ) {
                Some(true) => {}
                Some(false) => continue,
                None => {
                    *remaining_steps = 0;
                    return false;
                }
            }
            if !self.admit_ranked(index, query, limit, seen, ranked) {
                return false;
            }
        }
        true
    }

    /// Admit one entry under the common qualifier and de-duplication rules. `false` means the
    /// caller has filled the bounded response and must stop walking its current rung immediately.
    fn admit_ranked(
        &self,
        index: u32,
        query: &WorkspaceQuery,
        limit: usize,
        seen: &mut std::collections::HashSet<u32>,
        ranked: &mut Vec<u32>,
    ) -> bool {
        if ranked.len() >= limit {
            return false;
        }
        if self.qualifier_matches(index, query.package.as_deref()) && seen.insert(index) {
            ranked.push(index);
        }
        ranked.len() < limit
    }

    /// Entry indices whose key in `table` starts with `pattern`, by binary search over `order`.
    fn prefix_range<'a>(&self, order: &'a [u32], table: &'a [String], pattern: &str) -> &'a [u32] {
        let key = |index: &u32| self.name_of(*index, table);
        let start = order.partition_point(|index| key(index) < pattern);
        let count = order[start..].partition_point(|index| key(index).starts_with(pattern));
        &order[start..start + count]
    }

    fn simple_segment_prefix_range(&self, pattern: &str) -> &[u32] {
        let start = self
            .by_simple_segment
            .partition_point(|index| self.simple_segment(*index) < pattern);
        let count = self.by_simple_segment[start..]
            .partition_point(|index| self.simple_segment(*index).starts_with(pattern));
        &self.by_simple_segment[start..start + count]
    }

    fn name_of<'a>(&self, index: u32, table: &'a [String]) -> &'a str {
        self.entries
            .get(index as usize)
            .and_then(|entry| table.get(entry[0] as usize))
            .map(String::as_str)
            .unwrap_or_default()
    }

    fn matching_names(&self, predicate: impl Fn(&str, &str) -> bool) -> Vec<bool> {
        self.lowercase_names
            .iter()
            .enumerate()
            .map(|(id, name)| {
                let initials = self
                    .initials
                    .get(id)
                    .map(String::as_str)
                    .unwrap_or_default();
                predicate(name, initials)
            })
            .collect()
    }

    fn name_matches(&self, index: u32, matching: &[bool]) -> bool {
        self.entries
            .get(index as usize)
            .and_then(|entry| matching.get(entry[0] as usize))
            .copied()
            .unwrap_or(false)
    }

    /// Whether a qualified query's qualifier names this class's package or its outer classes.
    ///
    /// A qualified query matches a complete package suffix on a segment boundary, so
    /// `collections.AbstractList` finds `kotlin.collections` without admitting `kotlin.collectionsx`.
    ///
    /// The qualifier is also matched against the enclosing classes, because a nested class is
    /// spelled with the same separator: `Map.Entry` parses as the qualifier `map` and the name
    /// `Entry`, and a reader who typed it means `java.util.Map.Entry`, not a package called `map`.
    fn qualifier_matches(&self, index: u32, qualifier: Option<&str>) -> bool {
        let Some(qualifier) = qualifier else {
            return true;
        };
        let Some(entry) = self.entries.get(index as usize) else {
            return false;
        };
        let suffix_on_a_boundary = |candidate: &str| {
            let candidate = candidate.to_lowercase();
            candidate == qualifier
                || candidate
                    .strip_suffix(qualifier)
                    .is_some_and(|prefix| prefix.ends_with('.'))
        };
        let package_matches = self
            .packages
            .get(entry[1] as usize)
            .is_some_and(|declared| suffix_on_a_boundary(declared));
        package_matches
            || self
                .names
                .get(entry[0] as usize)
                .and_then(|name| name.rsplit_once('.'))
                .is_some_and(|(outer, _)| suffix_on_a_boundary(outer))
    }

    /// The segment a qualified query actually names: `Map.Entry` matches the entry whose simple
    /// name ends in `Entry`, not one literally called `Map.Entry`.
    fn simple_segment(&self, index: u32) -> &str {
        self.entries
            .get(index as usize)
            .and_then(|entry| self.lowercase_names.get(entry[0] as usize))
            .map(String::as_str)
            .map(|name| name.rsplit_once('.').map_or(name, |(_, last)| last))
            .unwrap_or_default()
    }

    fn candidate(&self, index: u32) -> Option<DependencyCandidate> {
        let entry = self.entries.get(index as usize)?;
        let package = self.packages.get(entry[1] as usize)?;
        let name = self.names.get(entry[0] as usize)?;
        Some(DependencyCandidate {
            internal: internal_name(package, name),
            package: package.clone(),
            name: name.clone(),
        })
    }
}

/// `(dotted package, simple name)` for a slashed internal name, or `None` when nothing would search
/// for it.
fn split_internal_name(internal: &str) -> Option<(String, String)> {
    // A multi-release jar carries whole duplicate trees under `META-INF/versions/<n>/`. They are
    // the same classes the root of the jar declares, and the classpath resolves them by their real
    // name, so indexing the versioned path yields a name that can never be located -- measured at
    // half of every candidate for a jar like byte-buddy.
    if internal.starts_with("META-INF/") {
        return None;
    }
    let (package, class) = match internal.rsplit_once('/') {
        Some((package, class)) => (package.replace('/', "."), class),
        None => (String::new(), internal),
    };
    if class.is_empty() {
        return None;
    }
    // Module and package descriptors are compiled entries but not declarations anyone searches for.
    if class == "module-info" || class == "package-info" {
        return None;
    }
    // `Foo$1`, `Foo$1$2`, `Foo$sam$...`: compiler-generated carriers, never searched for by name.
    if class.split('$').any(|segment| {
        segment.is_empty() || segment.chars().all(|character| character.is_ascii_digit())
    }) {
        return None;
    }
    Some((package, class.replace('$', ".")))
}

/// Every class one classpath entry declares.
fn jar_class_names(entry: &std::path::Path) -> Vec<String> {
    krusty::jvm::classpath::Classpath::new(vec![entry.to_path_buf()])
        .package_tree()
        .classes()
        .map(|(internal, _)| internal)
        .collect()
}

/// Rebuild the slashed internal name from the two parts it was split into.
///
/// The inverse of [`split_internal_name`], and exact: a package segment never contains `.`, and the
/// only `.` in a simple name is the nesting this introduced.
fn internal_name(package: &str, name: &str) -> String {
    let nested = name.replace('.', "$");
    if package.is_empty() {
        return nested;
    }
    format!("{}/{nested}", package.replace('.', "/"))
}

fn intern(value: &str, table: &mut Vec<String>, ids: &mut HashMap<String, u32>) -> u32 {
    if let Some(&id) = ids.get(value) {
        return id;
    }
    let id = table.len() as u32;
    table.push(value.to_string());
    ids.insert(value.to_string(), id);
    id
}

/// Where a dependency class was written out so a client can open it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedDependency {
    pub candidate: DependencyCandidate,
    pub path: std::path::PathBuf,
    /// Byte span of the declaration's name within the written text.
    pub span: krusty::diag::Span,
    pub text: String,
}

/// Write out the source for `candidates`, skipping any the classpath cannot resolve.
///
/// On demand and nowhere else: a class is rendered because a query is about to return it, never
/// because it exists. Rendering is cheap per class -- 20.7 µs measured -- but a project's
/// dependencies hold tens of thousands of them, and a reader asked about a handful.
pub fn locate(
    classpath: &std::rc::Rc<krusty::jvm::classpath::Classpath>,
    cache_root: &std::path::Path,
    candidates: Vec<DependencyCandidate>,
    use_sources: bool,
) -> Vec<LocatedDependency> {
    locate_with(cache_root, candidates, |candidate| {
        let source =
            crate::deps_render::materialize(classpath, &candidate.internal, "", "", use_sources)?;
        // No member name: a class-name query points at the class declaration itself.
        Some(source.into_text_and_span("", ""))
    })
}

/// Finish dependency locations using a host-provided semantic materializer.
///
/// The standalone renderer and the real worker use different transports to obtain source text,
/// but everything after that boundary must be identical: failed materialization is omitted, the
/// content-addressed cache is authoritative for the returned path, and the candidate/text/span are
/// kept together. Keeping that policy here prevents the production worker path from becoming an
/// untested second implementation of dependency location.
pub fn locate_with(
    cache_root: &std::path::Path,
    candidates: Vec<DependencyCandidate>,
    mut materialize: impl FnMut(&DependencyCandidate) -> Option<(String, krusty::diag::Span)>,
) -> Vec<LocatedDependency> {
    let mut located = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let Some((text, span)) = materialize(&candidate) else {
            continue;
        };
        let Ok(path) = crate::deps_cache::store(cache_root, &candidate.internal, &text) else {
            continue;
        };
        located.push(LocatedDependency {
            candidate,
            path,
            span,
            text,
        });
    }
    located
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(internals: &[&str]) -> DependencySymbolIndex {
        DependencySymbolIndex::from_internal_names(
            internals.iter().map(|internal| (*internal).to_string()),
        )
    }

    fn names(index: &DependencySymbolIndex, query: &str, limit: usize) -> Vec<String> {
        index
            .candidates(query, limit)
            .into_iter()
            .map(|candidate| candidate.name)
            .collect()
    }

    #[test]
    fn a_class_is_found_by_its_simple_name() {
        let index = index(&[
            "kotlin/collections/AbstractList",
            "java/lang/String",
            "java/util/List",
        ]);

        assert_eq!(names(&index, "AbstractList", 8), vec!["AbstractList"]);
        assert_eq!(names(&index, "abstractlist", 8), vec!["AbstractList"]);
        let candidate = index.candidates("String", 8).pop().unwrap();
        assert_eq!(candidate.internal, "java/lang/String");
        assert_eq!(candidate.package, "java.lang");
    }

    #[test]
    fn camel_humps_and_subsequences_rank_behind_prefixes() {
        let index = index(&[
            "demo/AbstractList",
            "demo/ArrayListDelegate",
            "demo/AbstractLinkedStack",
        ]);

        // The ladder keeps looking once a rung is exhausted, so weaker matches follow rather than
        // being excluded. What matters is which one comes first.
        let prefixed = names(&index, "abstractlist", 8);
        assert_eq!(prefixed.first().map(String::as_str), Some("AbstractList"));
        let initials = names(&index, "ald", 8);
        assert_eq!(
            initials.first().map(String::as_str),
            Some("ArrayListDelegate")
        );
    }

    #[test]
    fn a_nested_class_is_searchable_by_its_inner_name() {
        let index = index(&["java/util/Map$Entry", "kotlin/Result$Companion"]);

        // A reader looking for `Entry` means `Map.Entry`, so the dollar becomes a dot and the outer
        // class stays in the name.
        let candidate = index.candidates("Map.Entry", 8).pop().unwrap();
        assert_eq!(candidate.name, "Map.Entry");
        assert_eq!(candidate.internal, "java/util/Map$Entry");
        assert_eq!(names(&index, "Entry", 8), vec!["Map.Entry"]);
        assert_eq!(names(&index, "me", 8), vec!["Map.Entry"]);
    }

    #[test]
    fn multi_release_and_descriptor_entries_are_not_indexed() {
        let index = index(&[
            "net/bytebuddy/ByteBuddy",
            // A multi-release jar carries whole duplicate trees. The classpath resolves the real
            // name, so indexing the versioned path yields a class that can never be located.
            "META-INF/versions/9/net/bytebuddy/ByteBuddy",
            "META-INF/versions/11/net/bytebuddy/Other",
            "module-info",
            "demo/package-info",
        ]);

        assert_eq!(index.class_count(), 1);
        let found = index.candidates("ByteBuddy", 8);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].internal, "net/bytebuddy/ByteBuddy");
    }

    #[test]
    fn two_versions_of_one_artifact_yield_one_candidate() {
        // A classpath can carry two versions of the same jar; a name both declare is one class,
        // resolved from whichever entry comes first.
        let index = index(&[
            "kotlin/collections/AbstractList",
            "kotlin/collections/AbstractList",
        ]);

        assert_eq!(index.class_count(), 1);
        assert_eq!(index.candidates("AbstractList", 8).len(), 1);
    }

    #[test]
    fn synthetic_classes_are_not_indexed() {
        let index = index(&[
            "demo/Real",
            "demo/Real$1",
            "demo/Real$1$2",
            "demo/Real$sam$java_lang_Runnable$0",
        ]);

        assert_eq!(index.class_count(), 1);
        assert_eq!(names(&index, "Real", 8), vec!["Real"]);
    }

    #[test]
    fn a_qualified_query_selects_by_package() {
        let index = index(&[
            "kotlin/collections/Builder",
            "demo/app/Builder",
            "demo/collectionsx/Builder",
        ]);

        let found = index.candidates("collections.Builder", 8);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].package, "kotlin.collections");
        assert_eq!(names(&index, "Builder", 8).len(), 3);
    }

    #[test]
    fn ranking_stops_at_the_limit() {
        let internals = (0..64)
            .map(|index| format!("demo/Widget{index}"))
            .collect::<Vec<_>>();
        let index = DependencySymbolIndex::from_internal_names(internals);

        assert_eq!(index.candidates("Widget", 8).len(), 8);
        assert!(index.is_complete());
    }

    #[test]
    fn filtered_archive_entries_still_consume_the_bounded_scan_budget() {
        let index = DependencySymbolIndex::from_internal_names_with_limit(
            [
                "META-INF/versions/9/demo/Versioned".to_string(),
                "module-info".to_string(),
                "demo/WouldOtherwiseBeUnbounded".to_string(),
            ],
            2,
        );

        assert_eq!(index.class_count(), 0);
        assert!(!index.is_complete());
    }

    #[test]
    fn an_empty_query_returns_nothing() {
        let index = index(&["demo/Widget"]);

        // The dependency layer is the widest one there is; answering an empty query from it would
        // return an arbitrary slice of every jar on the classpath.
        assert!(index.candidates("", 8).is_empty());
    }

    #[test]
    fn wildcard_queries_share_the_workspace_query_grammar() {
        let index = index(&[
            "demo/AbstractList",
            "demo/AbstractMutableList",
            "demo/ArrayList",
        ]);

        // A trailing `::` is the shared protocol escape that disables the client's own literal
        // re-filter. The dependency layer used to parse the same request but silently treat `*` as
        // ordinary subsequence text, unlike the project layer below it.
        assert_eq!(
            names(&index, "Abstract*List::", 8),
            vec!["AbstractList", "AbstractMutableList"]
        );
    }

    #[test]
    fn dependency_queries_obey_the_shared_input_and_result_bounds() {
        let index = index(&["demo/Widget"]);
        let oversized = "W".repeat(crate::analysis::MAX_WORKSPACE_SYMBOL_QUERY_BYTES + 1);

        assert!(index.candidates(&oversized, 8).is_empty());
        assert!(index.candidates("Widget", 0).is_empty());
    }

    #[test]
    fn wildcard_transition_budget_is_shared_with_the_project_layer() {
        let project = crate::analysis::WorkspaceSymbolIndex::from_disk_sources(&[(
            "file:///Project.kt",
            "package demo\nclass RepeatedTarget\n",
        )]);
        let dependencies = index(&["vendor/RepeatedTarget"]);
        let parsed = WorkspaceQuery::parse("*target");
        let mut probe = usize::MAX;
        assert_eq!(
            matches_glob("repeatedtarget", &parsed.pattern, &mut probe),
            Some(true)
        );
        let mut remaining = usize::MAX - probe;

        let project_hits = project.encode_over_with_glob_steps(
            "*target",
            &[],
            &std::collections::HashSet::new(),
            &mut remaining,
        );
        let dependency_hits = dependencies.candidates_with_glob_steps("*target", 8, &mut remaining);

        assert_eq!(project_hits.len(), 1);
        assert!(dependency_hits.is_empty());
        assert_eq!(remaining, 0);
    }

    #[test]
    fn the_internal_name_survives_being_split_and_rebuilt() {
        // The index stores a package and a name and derives the internal name from them, which is
        // the largest table it would otherwise hold. That trade is only sound if it round-trips.
        for internal in [
            "kotlin/collections/AbstractList",
            "java/util/Map$Entry",
            "java/util/Map$Entry$Nested",
            "Rooted",
            "single/Segment",
        ] {
            let index = index(&[internal]);
            let candidate = index
                .candidates(&index.names[0].clone(), 4)
                .into_iter()
                .find(|candidate| candidate.internal == internal);
            assert!(
                candidate.is_some(),
                "{internal} did not survive the split and rebuild"
            );
        }
    }

    #[test]
    fn a_default_package_class_keeps_an_empty_package() {
        let index = index(&["Rooted"]);

        let candidate = index.candidates("Rooted", 8).pop().unwrap();
        assert_eq!(candidate.package, "");
        assert_eq!(candidate.internal, "Rooted");
    }
}
