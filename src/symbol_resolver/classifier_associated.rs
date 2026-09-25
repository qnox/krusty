//! Classifier-associated declarations: `companion { … }` block members and written
//! `companion fun/val C.name` declarations.
//!
//! Such a declaration is named through a classifier coordinate and has no value operand. Providers
//! publish it in the classifier's namespace as a receiver-less candidate carrying
//! `associated_classifier`; these queries collect those candidates for the two ways Kotlin names
//! them. A qualified `C.name` sees only `C`'s own declarations, while the static scope opened by a
//! class body or a companion-associated declaration also sees those of `C`'s supertypes, the nearer
//! classifier shadowing the farther one through each candidate's `receiver_rank`.

use crate::libraries::{FunctionInfo, PropertyInfo};
use crate::symbol_source::{SymbolNamespace, SymbolSource as _};
use crate::types::{Ty, TypeName};

use super::{direct_supertypes, SymbolResolver};

impl SymbolResolver<'_> {
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
        self.src
            .symbols(SymbolNamespace::Classifier(classifier), name)
            .callables
            .functions()
            .iter()
            .filter(|function| function.associated_classifier == Some(classifier))
            .filter(|function| self.non_member_callable_accessible(function))
            .cloned()
            .map(|mut function| {
                function.receiver_rank = rank;
                function
            })
            .collect()
    }

    fn associated_properties_of(
        &self,
        classifier: TypeName,
        name: &str,
        rank: u32,
    ) -> Vec<PropertyInfo> {
        let mut properties = self
            .src
            .symbols(SymbolNamespace::Classifier(classifier), name)
            .callables
            .properties()
            .iter()
            .filter(|property| property.associated_classifier == Some(classifier))
            .cloned()
            .collect::<Vec<_>>();
        properties.extend(
            self.lib
                .classifier_associated_property(classifier, name)
                .filter(|property| property.associated_classifier == Some(classifier)),
        );
        for property in &mut properties {
            property.receiver_rank = rank;
        }
        properties
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
