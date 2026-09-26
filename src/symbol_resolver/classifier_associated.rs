//! Classifier-associated declarations: `companion { … }` block members, written
//! `companion fun/val C.name` declarations, and the receiver-less properties a platform associates
//! with a classifier (a Java static field, a companion `@JvmField`).
//!
//! Such a declaration is named through a classifier coordinate and has no value operand. Providers
//! publish it in the classifier's namespace as a receiver-less candidate carrying
//! `associated_classifier`; these queries collect those candidates for the two ways Kotlin names
//! them. A qualified `C.name` sees only `C`'s own declarations, while the static scope opened by a
//! class body or a companion-associated declaration also sees those of `C`'s supertypes, the nearer
//! classifier shadowing the farther one through each candidate's `receiver_rank`.

use crate::assignable::{is_subtype, TyCtx};
use crate::libraries::{FunctionInfo, PropertyInfo};
use crate::symbol_source::{SymbolNamespace, SymbolSource as _};
use crate::types::{Ty, TypeName, Visibility};

use super::{direct_supertypes, SourceOracle, SymbolResolver};

impl SymbolResolver<'_> {
    /// The receiver-less property `internal.name` names: the one `internal`'s namespace record
    /// publishes as associated with it.
    pub fn associated_property(&self, internal: TypeName, name: &str) -> Option<PropertyInfo> {
        self.src
            .symbols(SymbolNamespace::Classifier(internal), name)
            .callables
            .properties()
            .iter()
            .find(|property| property.associated_classifier == Some(internal))
            .cloned()
    }

    /// Provider-normalized classifier property visible from this resolver's lexical access site.
    /// The declaration may be realized however the platform chooses; this operation deals only in
    /// Kotlin property shape and source visibility.
    pub(crate) fn accessible_classifier_associated_property(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<PropertyInfo> {
        self.associated_property(internal, name)
            .filter(|property| self.associated_property_accessible(property))
    }

    /// Whether an associated property's declaration is visible from this resolver's lexical
    /// access site. A current-module declaration is in this module; a dependency's `internal` one
    /// is visible only through the provider's friend-module rule, and its `private` one never.
    pub(crate) fn associated_property_accessible(&self, property: &PropertyInfo) -> bool {
        let current_module = property.source_key.is_some() || property.stable_declaration.is_some();
        match property.visibility {
            Visibility::Public => true,
            Visibility::Internal => {
                current_module
                    || self
                        .module
                        .is_some_and(|module| module.classifier(property.owner).is_some())
                    || self.lib.internal_accessible(property.owner)
            }
            Visibility::PackagePrivate => self.package_private_member_accessible(property.owner),
            Visibility::Private => property
                .associated_access_owner
                .map_or(current_module, |owner| self.lexically_inside(owner)),
            Visibility::Protected => self.lexical_classes.iter().copied().any(|enclosing| {
                is_subtype(
                    &TyCtx::new(),
                    &SourceOracle(&self.src),
                    Ty::obj_name(enclosing),
                    Ty::obj_name(property.owner),
                )
            }),
        }
    }

    /// The associated functions `classifier.name(…)` names: `classifier`'s own, accessible here.
    pub(crate) fn classifier_associated_callables(
        &self,
        classifier: TypeName,
        name: &str,
    ) -> Vec<FunctionInfo> {
        self.associated_functions_of(classifier, name, 0)
    }

    /// The associated property `classifier.name` names, if `classifier` declares one.
    pub(crate) fn classifier_associated_properties(
        &self,
        classifier: TypeName,
        name: &str,
    ) -> Vec<PropertyInfo> {
        self.associated_properties_of(classifier, name, 0)
    }

    /// The associated functions named `name` in `classifier`'s static scope: its own and its
    /// supertypes', each ranked by the supertype distance of its classifier.
    pub(crate) fn static_scope_associated_callables(
        &self,
        classifier: TypeName,
        name: &str,
    ) -> Vec<FunctionInfo> {
        self.static_scope_classifiers(classifier)
            .into_iter()
            .flat_map(|(owner, rank)| self.associated_functions_of(owner, name, rank))
            .collect()
    }

    /// The associated properties named `name` in `classifier`'s static scope, nearest classifier
    /// first.
    pub(crate) fn static_scope_associated_properties(
        &self,
        classifier: TypeName,
        name: &str,
    ) -> Vec<PropertyInfo> {
        self.static_scope_classifiers(classifier)
            .into_iter()
            .flat_map(|(owner, rank)| self.associated_properties_of(owner, name, rank))
            .collect()
    }

    fn associated_functions_of(
        &self,
        classifier: TypeName,
        name: &str,
        rank: u32,
    ) -> Vec<FunctionInfo> {
        self.associated_function_declarations(classifier, name)
            .into_iter()
            .filter(|function| self.associated_function_accessible(function))
            .map(|mut function| {
                function.receiver_rank = rank;
                function
            })
            .collect()
    }

    /// All declarations on this exact classifier coordinate, before lexical access filtering.
    /// Callers use this only to preserve the selected declaration's access diagnostic after the
    /// accessible family is empty; applicability and overload selection never see these entries.
    pub(crate) fn associated_function_declarations(
        &self,
        classifier: TypeName,
        name: &str,
    ) -> Vec<FunctionInfo> {
        self.src
            .symbols(SymbolNamespace::Classifier(classifier), name)
            .callables
            .functions()
            .iter()
            .filter(|function| function.associated_classifier == Some(classifier))
            .cloned()
            .collect()
    }

    pub(crate) fn associated_function_accessible(&self, function: &FunctionInfo) -> bool {
        match (function.visibility, function.associated_access_owner) {
            (Visibility::Private, Some(owner)) => self.lexically_inside(owner),
            _ => self.non_member_callable_accessible(function),
        }
    }

    fn lexically_inside(&self, owner: TypeName) -> bool {
        self.lexical_classes
            .iter()
            .copied()
            .any(|enclosing| enclosing.same_or_nested_within(owner))
    }

    fn associated_properties_of(
        &self,
        classifier: TypeName,
        name: &str,
        rank: u32,
    ) -> Vec<PropertyInfo> {
        // An enum entry's static field is published too, but the entry is not a property.
        if self
            .src
            .classifier(classifier)
            .is_some_and(|shape| shape.is_enum_entry(name))
        {
            return Vec::new();
        }
        self.src
            .symbols(SymbolNamespace::Classifier(classifier), name)
            .callables
            .properties()
            .iter()
            .filter(|property| property.associated_classifier == Some(classifier))
            .filter(|property| self.associated_property_accessible(property))
            .cloned()
            .map(|mut property| {
                property.receiver_rank = rank;
                property
            })
            .collect()
    }

    /// `classifier` and its supertypes, breadth first, each with its supertype distance.
    fn static_scope_classifiers(&self, classifier: TypeName) -> Vec<(TypeName, u32)> {
        let mut seen = std::collections::HashSet::from([classifier]);
        let mut classifiers = vec![(classifier, 0)];
        let mut frontier = vec![Ty::obj_name(classifier)];
        let mut rank = 1;
        while !frontier.is_empty() {
            let mut next = Vec::new();
            for supertype in frontier
                .into_iter()
                .flat_map(|current| direct_supertypes(&self.src, current))
            {
                let Some(owner) = supertype.kotlin_class_internal() else {
                    continue;
                };
                if seen.insert(owner) {
                    classifiers.push((owner, rank));
                    next.push(supertype);
                }
            }
            frontier = next;
            rank += 1;
        }
        classifiers
    }
}
