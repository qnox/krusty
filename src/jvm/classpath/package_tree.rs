//! Composition of per-entry package catalogs into one classpath-order package tree.
//!
//! Every stored `NameId` belongs to the tree retained by `PackageTree`. When composition adopts a
//! larger shared catalog tree, all identities accumulated so far are remapped before that tree is
//! published. Package and class order remain the classpath shadowing order.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::name_tree::{NameId, NameTree};
use crate::types::{existing_type_name_in, type_name_from, TypeName};

use super::{JarId, JarPackages};

#[derive(Clone, Default)]
struct PackageNode {
    jars: Vec<JarId>,
    builtins_jars: Vec<JarId>,
}

#[derive(Clone)]
pub struct PackageTree {
    names: Arc<NameTree>,
    packages: HashMap<NameId, PackageNode>,
    /// Package qualifiers that exist only as ancestors of a declared package.
    package_prefixes: HashSet<NameId>,
    /// Exact class owners, sorted by name identity and classpath order.
    classes: Vec<(NameId, JarId)>,
    incomplete_entries: Vec<JarId>,
}

impl Default for PackageTree {
    fn default() -> Self {
        Self {
            names: Arc::new(NameTree::default()),
            packages: HashMap::new(),
            package_prefixes: HashSet::new(),
            classes: Vec::new(),
            incomplete_entries: Vec::new(),
        }
    }
}

impl PackageTree {
    /// Compose entry catalogs in classpath order and normalize all order-sensitive collections.
    pub(super) fn compose(parts: &[Arc<JarPackages>]) -> Self {
        let mut tree = Self::default();
        for (entry, catalog) in parts.iter().enumerate() {
            tree.merge(entry, catalog);
        }
        tree.finish();
        tree
    }

    /// Merge one entry catalog under its exact classpath position.
    pub(super) fn merge(&mut self, entry: JarId, catalog: &JarPackages) {
        if !catalog.complete {
            self.incomplete_entries.push(entry);
        }
        if !Arc::ptr_eq(&self.names, &catalog.names)
            && catalog.names.node_count() > self.names.node_count()
        {
            self.adopt_name_tree(&catalog.names);
        }
        let shared_names = Arc::ptr_eq(&self.names, &catalog.names);
        for (&package_id, facts) in &catalog.packages {
            let package = if shared_names {
                package_id
            } else {
                self.names.insert_from(&catalog.names, package_id)
            };
            let node = self.packages.entry(package).or_default();
            if !node.jars.contains(&entry) {
                node.jars.push(entry);
                if node
                    .jars
                    .windows(2)
                    .next_back()
                    .is_some_and(|pair| pair[0] > pair[1])
                {
                    node.jars.sort_unstable();
                }
            }
            if facts.has_builtins && !node.builtins_jars.contains(&entry) {
                node.builtins_jars.push(entry);
                if node
                    .builtins_jars
                    .windows(2)
                    .next_back()
                    .is_some_and(|pair| pair[0] > pair[1])
                {
                    node.builtins_jars.sort_unstable();
                }
            }
            let mut ancestor = package;
            while let Some(parent) = self.names.parent(ancestor) {
                if parent == NameTree::ROOT || !self.package_prefixes.insert(parent) {
                    break;
                }
                ancestor = parent;
            }
        }
        for &class_id in &catalog.classes {
            let class = if shared_names {
                class_id
            } else {
                self.names.insert_from(&catalog.names, class_id)
            };
            self.classes.push((class, entry));
        }
    }

    /// Restore classpath-order invariants after the final merge.
    pub(super) fn finish(&mut self) {
        self.classes
            .sort_unstable_by_key(|&(class, entry)| (class.0, entry));
        self.classes.dedup();
        self.incomplete_entries.sort_unstable();
        self.incomplete_entries.dedup();
    }

    /// Every class the classpath declares, as its slashed internal name and owning entry.
    pub fn classes(&self) -> impl Iterator<Item = (String, JarId)> + '_ {
        self.classes
            .iter()
            .map(|(name, entry)| (self.names.render(*name), *entry))
    }

    pub fn has_package(&self, parent: TypeName, name: &str) -> bool {
        existing_type_name_in(&self.names, parent)
            .and_then(|parent| self.names.existing_child_of(parent, name))
            .is_some_and(|id| {
                self.packages.contains_key(&id) || self.package_prefixes.contains(&id)
            })
    }

    pub(in crate::jvm) fn catalog_complete(&self) -> bool {
        self.incomplete_entries.is_empty()
    }

    pub(super) fn incomplete_entries(&self) -> &[JarId] {
        &self.incomplete_entries
    }

    pub(super) fn entry_is_incomplete(&self, entry: JarId) -> bool {
        self.incomplete_entries.contains(&entry)
    }

    pub(super) fn package_count(&self) -> usize {
        self.packages.len()
    }

    pub(super) fn declares_package(&self, package: &str) -> bool {
        self.names
            .get(package)
            .is_some_and(|id| self.packages.contains_key(&id))
    }

    pub(super) fn declares_package_name(&self, package: TypeName) -> bool {
        existing_type_name_in(&self.names, package)
            .is_some_and(|id| self.packages.contains_key(&id))
    }

    pub(super) fn package_jars_name(&self, package: TypeName) -> Option<&[JarId]> {
        let package = existing_type_name_in(&self.names, package)?;
        Some(self.packages.get(&package)?.jars.as_slice())
    }

    pub(super) fn builtins_jars_name(&self, package: TypeName) -> Option<&[JarId]> {
        let package = existing_type_name_in(&self.names, package)?;
        Some(self.packages.get(&package)?.builtins_jars.as_slice())
    }

    pub(super) fn builtins_packages(&self) -> impl Iterator<Item = TypeName> + '_ {
        self.packages
            .iter()
            .filter(|(_, node)| !node.builtins_jars.is_empty())
            .map(|(&package, _)| type_name_from(&self.names, package))
    }

    pub(super) fn jars_for_class(&self, internal: &str) -> Vec<JarId> {
        let Some(class) = self.names.get(internal) else {
            return Vec::new();
        };
        self.jars_for_class_id(class)
    }

    pub(super) fn jars_for_class_name(&self, internal: TypeName) -> Vec<JarId> {
        let Some(class) = existing_type_name_in(&self.names, internal) else {
            return Vec::new();
        };
        self.jars_for_class_id(class)
    }

    pub(super) fn first_jar_for_spelling(&self, internal: &str) -> Option<JarId> {
        self.first_jar_for_id(self.names.get(internal)?)
    }

    pub(in crate::jvm) fn contains_exact_class(
        &self,
        package: TypeName,
        class_segment: &str,
    ) -> bool {
        let Some(parent) = existing_type_name_in(&self.names, package) else {
            return false;
        };
        let Some(class) = self.names.existing_child_of(parent, class_segment) else {
            return false;
        };
        self.first_jar_for_id(class).is_some()
    }

    pub(in crate::jvm) fn contains_nested_class(&self, owner: TypeName, nested: &str) -> bool {
        let Some(package) = existing_type_name_in(&self.names, owner.namespace()) else {
            return false;
        };
        let Some(class) = self
            .names
            .existing_nested_under(package, owner.segment_ref(), nested)
        else {
            return false;
        };
        self.first_jar_for_id(class).is_some()
    }

    fn first_jar_for_id(&self, class: NameId) -> Option<JarId> {
        let start = self
            .classes
            .partition_point(|&(candidate, _)| candidate.0 < class.0);
        self.classes
            .get(start)
            .and_then(|&(candidate, entry)| (candidate == class).then_some(entry))
    }

    fn jars_for_class_id(&self, class: NameId) -> Vec<JarId> {
        let start = self
            .classes
            .partition_point(|&(candidate, _)| candidate.0 < class.0);
        self.classes[start..]
            .iter()
            .take_while(|&&(candidate, _)| candidate == class)
            .map(|&(_, entry)| entry)
            .collect()
    }

    /// Adopt a larger catalog's tree and remap every identity accumulated so far.
    fn adopt_name_tree(&mut self, names: &Arc<NameTree>) {
        let old = Arc::clone(&self.names);
        self.names = Arc::clone(names);
        if old.node_count() <= 1 {
            return;
        }
        let remap = |id: NameId| names.insert_from(&old, id);
        self.packages = std::mem::take(&mut self.packages)
            .into_iter()
            .map(|(id, node)| (remap(id), node))
            .collect();
        self.package_prefixes = std::mem::take(&mut self.package_prefixes)
            .into_iter()
            .map(remap)
            .collect();
        for (class, _) in &mut self.classes {
            *class = remap(*class);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    fn catalog(
        packages: &[&str],
        classes: &[&str],
        complete: bool,
        builtins: &[&str],
    ) -> Arc<JarPackages> {
        let mut catalog = JarPackages {
            complete,
            ..JarPackages::default()
        };
        for package in packages {
            catalog.entry_mut(package).has_classes = true;
        }
        for package in builtins {
            catalog.entry_mut(package).has_builtins = true;
        }
        for class in classes {
            let id = catalog.names.insert(class);
            catalog.classes.push(id);
        }
        Arc::new(catalog)
    }

    #[test]
    fn larger_shared_tree_keeps_its_ids_and_remaps_existing_state() {
        let first = catalog(&["sample/base"], &["sample/base/Item"], true, &[]);
        let shared_names = Arc::new(NameTree::default());
        for name in [
            "sample/catalog/Widget",
            "sample/catalog/Extra",
            "sample/catalog/More",
        ] {
            shared_names.insert(name);
        }
        let shared_class = shared_names
            .get("sample/catalog/Widget")
            .expect("shared class identity");
        let mut second = JarPackages::with_names(Arc::clone(&shared_names));
        second.entry_mut("sample/catalog").has_classes = true;
        second.classes.push(shared_class);

        let tree = PackageTree::compose(&[first, Arc::new(second)]);

        assert!(Arc::ptr_eq(&tree.names, &shared_names));
        assert_eq!(tree.first_jar_for_id(shared_class), Some(1));
        assert_eq!(tree.jars_for_class("sample/base/Item"), vec![0]);
        assert_eq!(
            tree.package_jars_name(type_name("sample/base")),
            Some(&[0][..])
        );
        assert!(tree.has_package(TypeName::ROOT, "sample"));
    }

    #[test]
    fn base_plus_delta_matches_one_shot_order_and_completeness() {
        let first = catalog(
            &["sample/common"],
            &["sample/common/First", "sample/shared/Duplicate"],
            true,
            &[],
        );
        let middle = catalog(
            &["sample/local", "sample/common"],
            &["sample/local/Only", "sample/shared/Duplicate"],
            false,
            &["sample/common"],
        );
        let last = catalog(
            &["sample/catalog", "sample/common"],
            &["sample/catalog/Widget"],
            true,
            &["sample/common"],
        );

        let one_shot = PackageTree::compose(&[first.clone(), middle.clone(), last.clone()]);
        let mut delta = PackageTree::default();
        delta.merge(0, &first);
        delta.merge(2, &last);
        delta.finish();
        delta.merge(1, &middle);
        delta.finish();

        for package in ["sample/common", "sample/local", "sample/catalog"] {
            let package = type_name(package);
            assert_eq!(
                one_shot.package_jars_name(package),
                delta.package_jars_name(package)
            );
        }
        assert_eq!(
            delta.builtins_jars_name(type_name("sample/common")),
            Some(&[1, 2][..])
        );
        assert_eq!(
            one_shot.jars_for_class("sample/shared/Duplicate"),
            vec![0, 1]
        );
        assert_eq!(delta.jars_for_class("sample/shared/Duplicate"), vec![0, 1]);
        let render = |tree: &PackageTree| {
            let mut classes = tree.classes().collect::<Vec<_>>();
            classes.sort();
            classes
        };
        assert_eq!(render(&one_shot), render(&delta));
        assert_eq!(one_shot.incomplete_entries(), &[1]);
        assert_eq!(delta.incomplete_entries(), &[1]);
    }

    #[test]
    fn composition_unions_packages_and_routes_exact_classes() {
        let first = catalog(
            &["sample/shared"],
            &[
                "sample/shared/One",
                "sample/shared/Duplicate",
                "sample/shared/Outer$Nested",
            ],
            true,
            &[],
        );
        let second = catalog(
            &["sample/shared"],
            &["sample/shared/Two", "sample/shared/Duplicate"],
            true,
            &["sample/shared"],
        );
        let tree = PackageTree::compose(&[first, second]);

        assert_eq!(
            tree.package_jars_name(type_name("sample/shared")),
            Some(&[0, 1][..])
        );
        assert_eq!(tree.jars_for_class("sample/shared/One"), vec![0]);
        assert_eq!(tree.jars_for_class("sample/shared/Two"), vec![1]);
        assert_eq!(tree.jars_for_class("sample/shared/Duplicate"), vec![0, 1]);
        assert!(tree.contains_exact_class(type_name("sample/shared"), "Outer$Nested"));
        assert!(tree.contains_nested_class(type_name("sample/shared/Outer"), "Nested"));
        assert_eq!(tree.package_count(), 1);
    }
}
