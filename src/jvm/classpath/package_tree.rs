//! Composed classpath package catalog.
//!
//! A directory classpath shares the process-wide jar/jimage catalog and records only the directory
//! entries in its own layer. Lookups union the two. The shared catalog's name tree stays put.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::name_tree::{NameId, NameTree};
use crate::types::TypeName;

use super::{JarId, JarPackages};

/// A node in the composed classpath package table: every jar that declares THIS package (union across the
/// classpath, in declaration order). One jar sits in many package nodes.
#[derive(Clone, Default)]
pub struct PackageNode {
    jars: Vec<JarId>,
    /// Entries whose catalog records a `.kotlin_builtins` fragment for this package.
    builtins_jars: Vec<JarId>,
}

impl PackageNode {
    pub(super) fn jars(&self) -> &[JarId] {
        &self.jars
    }

    pub(super) fn builtins_jars(&self) -> &[JarId] {
        &self.builtins_jars
    }
}

#[derive(Clone, Default)]
pub struct PackageTree {
    names: NameTree,
    packages: HashMap<NameId, PackageNode>,
    /// Every package path that EXISTS as a qualifier, including the intermediate ones no jar declares
    /// directly. A catalog records only the packages that own class files, so `java/util` is a node
    /// while `java` — which owns none — is not; name resolution walking `java.util.ArrayList` segment
    /// by segment must still be able to answer "`java` is a package". Ancestors are folded in once at
    /// compose time rather than re-derived per query.
    package_prefixes: HashSet<NameId>,
    /// Exact class owners, sorted by name and classpath order.
    classes: Vec<(NameId, JarId)>,
    incomplete_entries: Vec<JarId>,
    /// Jar/jimage catalog this directory layer shares. `None` on a catalog that already contains
    /// every entry (a file-only classpath, or the shared base itself).
    base: Option<Arc<PackageTree>>,
}

impl PackageTree {
    /// Directory layer over a shared file catalog. The file catalog is not cloned.
    pub(super) fn sharing_base(base: Arc<Self>) -> Self {
        let incomplete_entries = base.incomplete_entries.clone();
        Self {
            incomplete_entries,
            base: Some(base),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(super) fn shared_base(&self) -> Option<&Arc<Self>> {
        self.base.as_ref()
    }

    #[cfg(test)]
    pub(super) fn names(&self) -> &NameTree {
        &self.names
    }

    pub(super) fn incomplete_entries(&self) -> &[JarId] {
        &self.incomplete_entries
    }

    /// Every class the classpath declares, as its slashed internal name and the jar that owns it.
    ///
    /// Sorted by name id and classpath order within one layer. WITHIN one name that is the shadowing
    /// order — the first entry to declare it wins resolution and iteration preserves that; the order of
    /// DISTINCT names is name-id (insertion) order, which a base+delta compose is free to permute.
    pub fn classes(&self) -> impl Iterator<Item = (String, JarId)> + '_ {
        let base = self.base.as_ref().map(|base| {
            base.classes
                .iter()
                .map(|(name, jar)| (base.names.render(*name), *jar))
        });
        let local = self
            .classes
            .iter()
            .map(|(name, jar)| (self.names.render(*name), *jar));
        base.into_iter().flatten().chain(local)
    }

    /// The node for a slashed package path (`""` = this root), or `None` if no jar declares it.
    pub(super) fn node_for(&self, pkg: &str) -> Option<&PackageNode> {
        if let Some(base) = &self.base {
            if !self.layer_could_contain_spelling(pkg) {
                return base.node_for(pkg);
            }
        }
        self.names
            .get(pkg)
            .and_then(|id| self.packages.get(&id))
            .or_else(|| self.base.as_ref().and_then(|base| base.node_for(pkg)))
    }

    pub(super) fn node_for_name(&self, pkg: TypeName) -> Option<&PackageNode> {
        if self.base.is_some() && !self.layer_could_contain(pkg) {
            return self.base.as_ref().and_then(|base| base.node_for_name(pkg));
        }
        crate::types::existing_type_name_in(&self.names, pkg)
            .and_then(|id| self.packages.get(&id))
            .or_else(|| self.base.as_ref().and_then(|base| base.node_for_name(pkg)))
    }

    /// Whether a slashed path names a PACKAGE on this classpath — the qualifier half of name
    /// resolution. A dotted reference is resolved segment by segment, and each prefix is either a
    /// package, a classifier, or nothing; only this table can answer the first case, so an intermediate
    /// package that owns no classes of its own (`java`, `kotlin/collections`' parent) answers `true`
    /// here as well as a leaf one.
    pub fn has_package(&self, parent: TypeName, name: &str) -> bool {
        self.layer_has_package(parent, name)
            || self
                .base
                .as_ref()
                .is_some_and(|base| base.has_package(parent, name))
    }

    pub(super) fn jars_for_class(&self, internal: &str) -> Vec<JarId> {
        let Some(base) = &self.base else {
            return self.jars_in_layer(internal);
        };
        if !self.layer_could_contain_spelling(internal) {
            return base.jars_for_class(internal);
        }
        let mut jars = self.jars_in_layer(internal);
        if jars.is_empty() {
            return base.jars_for_class(internal);
        }
        let more = base.jars_for_class(internal);
        if more.is_empty() {
            return jars;
        }
        jars.extend(more);
        jars.sort_unstable();
        jars.dedup();
        jars
    }

    pub(super) fn jars_for_class_name(&self, internal: TypeName) -> Vec<JarId> {
        let Some(base) = &self.base else {
            return self.jars_in_layer_name(internal);
        };
        if !self.layer_could_contain(internal) {
            return base.jars_for_class_name(internal);
        }
        let mut jars = self.jars_in_layer_name(internal);
        if jars.is_empty() {
            return base.jars_for_class_name(internal);
        }
        let more = base.jars_for_class_name(internal);
        if more.is_empty() {
            return jars;
        }
        jars.extend(more);
        jars.sort_unstable();
        jars.dedup();
        jars
    }

    /// Packages whose catalog records a `.kotlin_builtins` fragment, as global type names.
    /// A shared file catalog and a directory layer both contribute; a package in both is once.
    pub(super) fn builtin_packages(&self) -> Vec<TypeName> {
        let mut packages = Vec::new();
        self.collect_builtin_packages(&mut packages);
        packages
    }

    fn collect_builtin_packages(&self, packages: &mut Vec<TypeName>) {
        if let Some(base) = &self.base {
            base.collect_builtin_packages(packages);
        }
        for (&package, node) in &self.packages {
            if node.builtins_jars.is_empty() {
                continue;
            }
            let name = crate::types::type_name_from(&self.names, package);
            if !packages.contains(&name) {
                packages.push(name);
            }
        }
    }

    /// Total package count in the table. For memory reporting.
    pub(super) fn package_count(&self) -> usize {
        let inherited = self.base.as_ref().map_or(0, |base| base.package_count());
        let added = self
            .packages
            .keys()
            .filter(|id| {
                self.base.as_ref().is_none_or(|base| {
                    base.names
                        .existing_from(&self.names, **id)
                        .is_none_or(|base_id| !base.packages.contains_key(&base_id))
                })
            })
            .count();
        inherited + added
    }

    fn layer_has_package(&self, parent: TypeName, name: &str) -> bool {
        if self.base.is_some() && !self.layer_could_contain(parent) {
            return false;
        }
        crate::types::existing_type_name_in(&self.names, parent)
            .and_then(|parent| self.names.existing_child_of(parent, name))
            .is_some_and(|id| {
                self.packages.contains_key(&id) || self.package_prefixes.contains(&id)
            })
    }

    /// `false` when this layer's names cannot hold `pkg`, so the caller should read the shared catalog
    /// and skip a path walk. An empty spelling (the default package) always might.
    fn layer_could_contain(&self, pkg: TypeName) -> bool {
        if pkg == TypeName::ROOT {
            return true;
        }
        let root = crate::types::type_name_root_segment(pkg);
        !root.is_empty() && self.names.existing_child_of(NameTree::ROOT, root).is_some()
    }

    fn layer_could_contain_spelling(&self, pkg: &str) -> bool {
        let top = pkg.split_once('/').map(|(head, _)| head).unwrap_or(pkg);
        !top.is_empty() && self.names.existing_child_of(NameTree::ROOT, top).is_some()
    }

    fn jars_in_layer(&self, internal: &str) -> Vec<JarId> {
        let Some(class) = self.names.get(internal) else {
            return Vec::new();
        };
        self.jars_for_class_id(class)
    }

    fn jars_in_layer_name(&self, internal: TypeName) -> Vec<JarId> {
        let Some(class) = crate::types::existing_type_name_in(&self.names, internal) else {
            return Vec::new();
        };
        self.jars_for_class_id(class)
    }

    fn jars_for_class_id(&self, class: NameId) -> Vec<JarId> {
        let start = self
            .classes
            .partition_point(|&(candidate, _)| candidate.0 < class.0);
        self.classes[start..]
            .iter()
            .take_while(|&&(candidate, _)| candidate == class)
            .map(|&(_, jar)| jar)
            .collect()
    }

    /// Copy the shared catalog's node for `pkg` into this layer so a directory entry can extend it.
    /// Name ids differ between the two trees; the match is by path, not by id.
    fn adopt_base_package(&mut self, pkg: NameId) {
        if self.packages.contains_key(&pkg) {
            return;
        }
        let Some(node) = self.base.as_ref().and_then(|base| {
            let base_id = base.names.existing_from(&self.names, pkg)?;
            base.packages.get(&base_id).cloned()
        }) else {
            return;
        };
        self.packages.insert(pkg, node);
    }
}

/// Compose per-jar [`JarPackages`] into the merged [`PackageTree`] — a cheap union: every package a jar
/// declares adds that jar to the package's node (in classpath declaration order).
pub(super) fn compose_package_tree(parts: &[Arc<JarPackages>]) -> PackageTree {
    let mut tree = PackageTree::default();
    for (jar_id, jp) in parts.iter().enumerate() {
        merge_package_tree_part(&mut tree, jar_id, jp);
    }
    finish_package_tree(&mut tree);
    tree
}

/// Merge ONE entry's catalog into a composed tree under an EXPLICIT entry id. The id is the
/// entry's position on the classpath: compose order and entry indices must agree, because a
/// package node's `jars` order IS the shadowing order lookups walk. When a part is merged out of
/// position order (the base+delta path), the touched vectors are re-sorted so the result is
/// indistinguishable from a single in-order compose.
pub(super) fn merge_package_tree_part(tree: &mut PackageTree, jar_id: JarId, jp: &JarPackages) {
    if !jp.complete {
        tree.incomplete_entries.push(jar_id);
    }
    for (&pkg_id, entry) in &jp.packages {
        let pkg = tree.names.insert_from(&jp.names, pkg_id);
        tree.adopt_base_package(pkg);
        let node = tree.packages.entry(pkg).or_default();
        if !node.jars.contains(&jar_id) {
            node.jars.push(jar_id);
            if node
                .jars
                .windows(2)
                .next_back()
                .is_some_and(|w| w[0] > w[1])
            {
                node.jars.sort_unstable();
            }
        }
        if entry.has_builtins && !node.builtins_jars.contains(&jar_id) {
            node.builtins_jars.push(jar_id);
            if node
                .builtins_jars
                .windows(2)
                .next_back()
                .is_some_and(|w| w[0] > w[1])
            {
                node.builtins_jars.sort_unstable();
            }
        }
        // Every ancestor of a declared package is itself a package qualifier, even when no jar
        // declares it directly.
        let mut ancestor = pkg;
        while let Some(parent) = tree.names.parent(ancestor) {
            if parent == NameTree::ROOT || !tree.package_prefixes.insert(parent) {
                break;
            }
            ancestor = parent;
        }
    }
    for &class_id in &jp.classes {
        let class = tree.names.insert_from(&jp.names, class_id);
        tree.classes.push((class, jar_id));
    }
}

/// Order-normalize a composed tree after the last part: exactly the tail of the original one-shot
/// compose, plus `incomplete_entries` ordering (the one-shot loop produced it ascending for free).
pub(super) fn finish_package_tree(tree: &mut PackageTree) {
    tree.classes
        .sort_unstable_by_key(|&(class, jar)| (class.0, jar));
    tree.classes.dedup();
    tree.incomplete_entries.sort_unstable();
}
