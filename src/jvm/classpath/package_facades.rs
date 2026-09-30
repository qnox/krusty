//! Public multifile facades of one package, remembered for the classpath snapshot.

use super::Classpath;
use crate::types::{type_name, type_name_from_multifile_facade, TypeName};

impl Classpath {
    /// The public multifile facades a package declares, from the `kotlin_module` catalog (the parts
    /// `…Kt__X` collapsed to their public facade `…Kt`), across every jar that declares the package.
    /// Declaration order, deduped. The list is fixed for this classpath, so a later lookup clones it.
    pub fn package_facades(&self, pkg: &str) -> Vec<TypeName> {
        self.package_facades_name(type_name(pkg))
    }

    pub fn package_facades_name(&self, pkg: TypeName) -> Vec<TypeName> {
        if let Some(cached) = self.package_facades_memo.borrow().get(&pkg) {
            return cached.clone();
        }
        let tree = self.package_tree();
        let mut out = Vec::new();
        if let Some(node) = tree.node_for_name(pkg) {
            for &jar_id in node.jars() {
                if self.entries.get(jar_id).is_none() {
                    continue;
                }
                let packages = self.entry_packages(jar_id);
                let part_ids = packages
                    .entry_name(pkg)
                    .map(|entry| entry.facades.clone())
                    .unwrap_or_default();
                for part_id in part_ids {
                    let facade = type_name_from_multifile_facade(&packages.names, part_id);
                    if !out.contains(&facade) {
                        out.push(facade);
                    }
                }
            }
        }
        self.package_facades_memo
            .borrow_mut()
            .insert(pkg, out.clone());
        out
    }
}
