//! Stable lookup of the declared bounds that do not fit in `Ty::TyParam`.
//!
//! A type-parameter type carries its first upper bound. Assignability needs the rest by the same
//! declaration-scoped semantic identity, including during Pass 1 before resolved headers exist.

use std::collections::HashMap;

use crate::types::Ty;

use super::{ClassSig, SymbolTable};

#[derive(Default)]
pub(super) struct ExtraBoundsIndex {
    by_parameter: HashMap<String, Vec<Ty>>,
}

impl ExtraBoundsIndex {
    pub(super) fn record_classifier(&mut self, classifier: &ClassSig) {
        for (parameter, extra) in classifier
            .type_params()
            .iter()
            .zip(&classifier.type_parameter_extra_bounds)
        {
            if !extra.is_empty() {
                self.by_parameter.insert(parameter.clone(), extra.clone());
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
                        .insert(formal.clone(), bounds[1..].to_vec());
                }
            }
        }
        index
    }

    pub(super) fn extend(&mut self, other: Self) {
        self.by_parameter.extend(other.by_parameter);
    }

    pub(super) fn get(&self, identity: &str) -> Vec<Ty> {
        self.by_parameter.get(identity).cloned().unwrap_or_default()
    }
}
