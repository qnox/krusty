//! A source classifier's applied supertype hierarchy: its direct parents in declaration order,
//! with the class's type arguments substituted, and the walks over them.

use crate::fx_hash::HashMap;

use super::{ClassSig, SymbolTable};
use crate::types::{Ty, TypeName};

/// Direct source-parent templates, before applying any receiver arguments. Consumers that must
/// approximate projected arguments positionally cannot first erase that evidence by substitution.
pub(super) fn source_parent_templates(class: &ClassSig) -> Vec<(TypeName, Ty)> {
    let mut parents = class
        .interfaces
        .iter_ids()
        .enumerate()
        .map(|(index, parent)| {
            let arguments = class
                .interface_type_args
                .get(index)
                .map(Vec::as_slice)
                .unwrap_or_default();
            (parent, Ty::obj_args_name(parent, arguments))
        })
        .collect::<Vec<_>>();
    if let Some(parent) = class.super_internal {
        parents.insert(
            class.interfaces_before_superclass as usize,
            (parent, Ty::obj_args_name(parent, &class.super_type_args)),
        );
    }
    parents
}

impl SymbolTable {
    /// `class`'s direct parents applied under `bindings`, in declaration order: the superclass
    /// keeps its written slot among the interfaces.
    pub(crate) fn applied_source_parents(
        &self,
        class: &ClassSig,
        bindings: HashMap<String, Ty>,
    ) -> Vec<(TypeName, Ty)> {
        source_parent_templates(class)
            .into_iter()
            .map(|(owner, template)| (owner, crate::symbol_resolver::ty_subst(template, &bindings)))
            .collect()
    }

    /// Applied source hierarchy, including its first external boundary.
    pub(crate) fn applied_type_hierarchy(&self, root: Ty) -> Vec<(TypeName, Ty, usize)> {
        let Some(internal) = root.obj_internal() else {
            return Vec::new();
        };
        let mut pending = vec![(internal, root, 0)];
        let mut seen = crate::fx_hash::HashSet::default();
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
        let mut seen = crate::fx_hash::HashSet::default();
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
