//! A source classifier's applied supertype hierarchy: its direct parents in declaration order,
//! with the class's type arguments substituted, and the walks over them.

use std::collections::HashMap;

use super::{ClassSig, SymbolTable};
use crate::types::{Ty, TypeName};

impl SymbolTable {
    /// `class`'s direct parents applied under `bindings`, in declaration order: the superclass
    /// keeps its written slot among the interfaces.
    pub(crate) fn applied_source_parents(
        &self,
        class: &ClassSig,
        bindings: HashMap<String, Ty>,
    ) -> Vec<(TypeName, Ty)> {
        let mut parents = class
            .interfaces
            .iter_ids()
            .enumerate()
            .map(|(index, parent)| {
                let arguments = class
                    .interface_type_args
                    .get(index)
                    .map(|arguments| {
                        arguments
                            .iter()
                            .map(|shape| crate::symbol_resolver::ty_subst(*shape, &bindings))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                (parent, Ty::obj_args_name(parent, &arguments))
            })
            .collect::<Vec<_>>();
        if let Some(parent) = class.super_internal {
            let arguments = class
                .super_type_args
                .iter()
                .map(|shape| crate::symbol_resolver::ty_subst(*shape, &bindings))
                .collect::<Vec<_>>();
            parents.insert(
                class.interfaces_before_superclass as usize,
                (parent, Ty::obj_args_name(parent, &arguments)),
            );
        }
        parents
    }

    /// Applied source hierarchy, including its first external boundary.
    pub(crate) fn applied_type_hierarchy(&self, root: Ty) -> Vec<(TypeName, Ty, usize)> {
        let Some(internal) = root.obj_internal() else {
            return Vec::new();
        };
        let mut pending = vec![(internal, root, 0)];
        let mut seen = std::collections::HashSet::new();
        let mut hierarchy = Vec::new();
        while let Some((owner, applied, depth)) = pending.pop() {
            if !seen.insert(owner) {
                continue;
            }
            hierarchy.push((owner, applied, depth));
            if let Some(class) = self.class_by_type_name(owner) {
                let bindings = class.type_parameter_bindings(applied);
                pending.extend(
                    self.applied_source_parents(class, bindings)
                        .into_iter()
                        .map(|(parent, applied)| (parent, applied, depth + 1)),
                );
            }
        }
        hierarchy
    }

    pub(crate) fn applied_hierarchy(&self, root: Ty) -> Vec<(TypeName, Ty, usize)> {
        let Some(internal) = root.obj_internal() else {
            return Vec::new();
        };
        let mut pending = std::collections::VecDeque::from([(internal, root, 0)]);
        let mut seen = std::collections::HashSet::new();
        let mut hierarchy = Vec::new();
        while let Some((owner, applied, depth)) = pending.pop_front() {
            if !seen.insert(owner) {
                continue;
            }
            hierarchy.push((owner, applied, depth));
            let parents = if let Some(class) = self.class_by_type_name(owner) {
                self.applied_source_parents(class, class.type_parameter_bindings(applied))
                    .into_iter()
                    .map(|(_, ty)| ty)
                    .collect()
            } else {
                crate::symbol_resolver::direct_supertypes(&*self.libraries, applied)
            };
            let parents = crate::symbol_resolver::with_implicit_any(owner, parents);
            pending.extend(
                parents
                    .into_iter()
                    .filter_map(|parent| Some((parent.obj_internal()?, parent, depth + 1))),
            );
        }
        hierarchy
    }
}
