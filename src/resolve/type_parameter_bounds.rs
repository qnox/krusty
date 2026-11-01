//! Stable lookup of every declared bound of a type parameter, which `Ty::TyParam` cannot carry.
//!
//! A type-parameter type carries one upper bound, its first written one. Assignability therefore
//! needs all declared bounds by the declaration-scoped semantic identity, including during Pass 1
//! before resolved headers exist. Which bound a target erases the parameter to is the backend's
//! choice, made from the declaration's complete bounds.

use crate::types::TyKind;
use crate::{fx_hash::HashMap, types::Ty};

use super::{ClassSig, SymbolTable, TParams};

/// Bound inventory for body-local declarations, which have semantic identities but no stable
/// module headers. The checker records a local function's own parameters before checking its body;
/// globally unique identities let nested bodies retain enclosing entries without scope-name lookup.
#[derive(Default)]
pub(super) struct LexicalBoundsIndex {
    by_parameter: HashMap<String, Vec<Ty>>,
}

impl LexicalBoundsIndex {
    pub(super) fn record(&mut self, names: &[String], parameters: &TParams) {
        for source_name in names {
            let parameter = parameters.bound(source_name);
            let TyKind::TyParam(identity, first_bound) = parameter.non_null().kind() else {
                continue;
            };
            let extra = parameters.extra_bounds_of(source_name);
            if !extra.is_empty() {
                self.by_parameter.insert(
                    identity.to_string(),
                    std::iter::once(first_bound).chain(extra).collect(),
                );
            }
        }
    }

    pub(super) fn all(&self, identity: &str) -> Option<Vec<Ty>> {
        self.by_parameter.get(identity).cloned()
    }
}

#[derive(Default)]
pub(super) struct DeclaredBoundsIndex {
    by_parameter: HashMap<String, Vec<Ty>>,
}

impl DeclaredBoundsIndex {
    pub(super) fn record_classifier(&mut self, classifier: &ClassSig) {
        for ((parameter, &first), extra) in classifier
            .type_params()
            .iter()
            .zip(classifier.type_param_bounds())
            .zip(&classifier.type_parameter_extra_bounds)
        {
            if !extra.is_empty() {
                let bounds = std::iter::once(first)
                    .chain(extra.iter().copied())
                    .collect();
                self.by_parameter.insert(parameter.clone(), bounds);
            }
        }
    }

    pub(super) fn from_callables(table: &SymbolTable) -> Self {
        let mut index = Self::default();
        let signatures = table
            .funs
            .values()
            .flatten()
            .chain(table.ext_funs.values().flat_map(HashMap::values).flatten())
            .chain(
                table
                    .classes
                    .values()
                    .flat_map(|class| class.methods.values().flatten()),
            )
            .chain(table.classes.values().flat_map(|class| {
                class
                    .member_ext_funs
                    .values()
                    .flatten()
                    .map(super::MemberExtFunSig::signature)
            }));
        for generic in signatures.filter_map(|signature| signature.generic_sig.as_ref()) {
            for (formal, bounds) in generic.formals.iter().zip(&generic.formal_bounds) {
                if bounds.len() > 1 {
                    index
                        .by_parameter
                        .insert(formal.to_string(), bounds.clone());
                }
            }
        }
        index
    }

    pub(super) fn extend(&mut self, other: Self) {
        self.by_parameter.extend(other.by_parameter);
    }

    /// Every declared bound, or none when the parameter declares at most one.
    pub(super) fn all(&self, identity: &str) -> Vec<Ty> {
        self.by_parameter.get(identity).cloned().unwrap_or_default()
    }

    /// The declared bounds after the first.
    pub(super) fn extra(&self, identity: &str) -> Vec<Ty> {
        self.by_parameter
            .get(identity)
            .map(|bounds| bounds[1..].to_vec())
            .unwrap_or_default()
    }
}
