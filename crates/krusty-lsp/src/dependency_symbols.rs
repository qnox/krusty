//! Searchable class names from the classpath.
//!
//! The project index answers for the workspace's own sources. This answers for everything they
//! depend on — the stdlib, every jar Gradle resolved, the JDK image — which is where most of the
//! names a reader looks up actually live.
//!
//! It holds names only. A dependency symbol has no location until its class is rendered back to
//! source, and rendering every class up front would be work nobody asked for: measured, a class
//! renders in 20.7 µs, so the few a query returns cost well under a millisecond, while the whole
//! local Gradle cache (3,456 jars) would be minutes. Ranking therefore happens here, over names,
//! and only the survivors are ever rendered.

use std::collections::HashMap;

use crate::analysis::{
    camel_hump_initials, is_ordered_subsequence_lowercase, qwerty_from_cyrillic, WorkspaceQuery,
};

/// Ceiling on classes retained per classpath. The local Gradle cache holds 3,456 jars and the doc's
/// measurement of every class in it is 436,289 names — an index this size is a name table, not a
/// problem, but a workspace that somehow exceeds it stops growing rather than the process.
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
    /// `(name id, package id, internal id)`, one per class.
    entries: Vec<[u32; 3]>,
    names: Vec<String>,
    lowercase_names: Vec<String>,
    initials: Vec<String>,
    packages: Vec<String>,
    internals: Vec<String>,
    by_name: Vec<u32>,
    by_initials: Vec<u32>,
    complete: bool,
}

impl DependencySymbolIndex {
    /// Build from slashed internal names, as `Classpath::package_tree().classes()` yields them.
    ///
    /// A `$` in an internal name is a nested class, and a reader searching for `Entry` means
    /// `Map.Entry`, so the separator becomes `.` and the simple name keeps its outer classes.
    /// Synthetic names — anonymous classes, lambda carriers — are dropped: nobody searches for
    /// `Foo$1`, and they outnumber the real declarations in some jars.
    pub fn from_internal_names(internals: impl IntoIterator<Item = String>) -> Self {
        let mut result = Self {
            complete: true,
            ..Self::default()
        };
        let mut name_ids = HashMap::<String, u32>::new();
        let mut package_ids = HashMap::<String, u32>::new();
        for internal in internals {
            if result.entries.len() >= MAX_DEPENDENCY_CLASSES {
                result.complete = false;
                break;
            }
            let Some((package, simple)) = split_internal_name(&internal) else {
                continue;
            };
            let name_id = intern(&simple, &mut result.names, &mut name_ids);
            let package_id = intern(&package, &mut result.packages, &mut package_ids);
            let internal_id = result.internals.len() as u32;
            result.internals.push(internal);
            result.entries.push([name_id, package_id, internal_id]);
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
        let key = |index: &u32, table: &[String]| {
            self.entries
                .get(*index as usize)
                .and_then(|entry| table.get(entry[0] as usize))
                .map(String::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let mut by_name = (0..self.entries.len() as u32).collect::<Vec<u32>>();
        by_name.sort_unstable_by(|left, right| {
            key(left, &self.lowercase_names)
                .cmp(&key(right, &self.lowercase_names))
                .then_with(|| left.cmp(right))
        });
        let mut by_initials = (0..self.entries.len() as u32).collect::<Vec<u32>>();
        by_initials.sort_unstable_by(|left, right| {
            key(left, &self.initials)
                .cmp(&key(right, &self.initials))
                .then_with(|| left.cmp(right))
        });
        self.by_name = by_name;
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
        let mut parsed = vec![WorkspaceQuery::parse(query)];
        if let Some(latin) = qwerty_from_cyrillic(query) {
            let translated = WorkspaceQuery::parse(&latin);
            if translated != parsed[0] {
                parsed.push(translated);
            }
        }
        let mut ranked = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for query in &parsed {
            for index in self.ranked_indices(query, limit) {
                if ranked.len() >= limit {
                    break;
                }
                if !seen.insert(index) {
                    continue;
                }
                if let Some(candidate) = self.candidate(index) {
                    ranked.push(candidate);
                }
            }
        }
        ranked
    }

    fn ranked_indices(&self, query: &WorkspaceQuery, limit: usize) -> Vec<u32> {
        let pattern = &query.pattern;
        if pattern.is_empty() {
            return Vec::new();
        }
        let mut ranked = Vec::new();
        let push = |ranked: &mut Vec<u32>, index: u32| {
            if self.qualifier_matches(index, query.package.as_deref()) {
                ranked.push(index);
            }
        };
        for &index in self.prefix_range(&self.by_name, &self.lowercase_names, pattern) {
            push(&mut ranked, index);
        }
        if query.package.is_some() {
            // A qualified query spells the qualifier separately, so the pattern is the last segment
            // and a nested class has to be reachable by it.
            for index in 0..self.entries.len() as u32 {
                if self.simple_segment(index).starts_with(pattern)
                    && !self
                        .name_of(index, &self.lowercase_names)
                        .starts_with(pattern)
                {
                    push(&mut ranked, index);
                }
            }
        }
        if ranked.len() < limit {
            for &index in self.prefix_range(&self.by_initials, &self.initials, pattern) {
                if !self
                    .name_of(index, &self.lowercase_names)
                    .starts_with(pattern)
                {
                    push(&mut ranked, index);
                }
            }
        }
        if ranked.len() < limit {
            let matching = self.matching_names(|name, initials| {
                is_ordered_subsequence_lowercase(initials, pattern)
                    && !initials.starts_with(pattern)
                    && !name.starts_with(pattern)
            });
            for index in 0..self.entries.len() as u32 {
                if self.name_matches(index, &matching) {
                    push(&mut ranked, index);
                }
            }
        }
        if ranked.len() < limit {
            let matching = self.matching_names(|name, initials| {
                is_ordered_subsequence_lowercase(name, pattern)
                    && !name.starts_with(pattern)
                    && !is_ordered_subsequence_lowercase(initials, pattern)
            });
            for index in 0..self.entries.len() as u32 {
                if self.name_matches(index, &matching) {
                    push(&mut ranked, index);
                }
            }
        }
        ranked
    }

    /// Entry indices whose key in `table` starts with `pattern`, by binary search over `order`.
    fn prefix_range<'a>(&self, order: &'a [u32], table: &'a [String], pattern: &str) -> &'a [u32] {
        let key = |index: &u32| self.name_of(*index, table);
        let start = order.partition_point(|index| key(index) < pattern);
        let count = order[start..].partition_point(|index| key(index).starts_with(pattern));
        &order[start..start + count]
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
        Some(DependencyCandidate {
            internal: self.internals.get(entry[2] as usize)?.clone(),
            package: self.packages.get(entry[1] as usize)?.clone(),
            name: self.names.get(entry[0] as usize)?.clone(),
        })
    }
}

/// `(dotted package, simple name)` for a slashed internal name, or `None` when nothing would search
/// for it.
fn split_internal_name(internal: &str) -> Option<(String, String)> {
    let (package, class) = match internal.rsplit_once('/') {
        Some((package, class)) => (package.replace('/', "."), class),
        None => (String::new(), internal),
    };
    if class.is_empty() {
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

fn intern(value: &str, table: &mut Vec<String>, ids: &mut HashMap<String, u32>) -> u32 {
    if let Some(&id) = ids.get(value) {
        return id;
    }
    let id = table.len() as u32;
    table.push(value.to_string());
    ids.insert(value.to_string(), id);
    id
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
        assert_eq!(names(&index, "me", 8), vec!["Map.Entry"]);
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
    fn an_empty_query_returns_nothing() {
        let index = index(&["demo/Widget"]);

        // The dependency layer is the widest one there is; answering an empty query from it would
        // return an arbitrary slice of every jar on the classpath.
        assert!(index.candidates("", 8).is_empty());
    }

    #[test]
    fn a_default_package_class_keeps_an_empty_package() {
        let index = index(&["Rooted"]);

        let candidate = index.candidates("Rooted", 8).pop().unwrap();
        assert_eq!(candidate.package, "");
        assert_eq!(candidate.internal, "Rooted");
    }
}
