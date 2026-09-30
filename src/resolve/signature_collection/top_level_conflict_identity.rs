//! Kotlin overload shape of one top-level callable.
//!
//! Type-parameter arity is part of that shape, so `fun <T> f(): Int` and `fun f(): Int` stay
//! independent declarations. An equal physical JVM signature is a platform fact diagnosed after
//! representation, not a resolver conflict.

use super::*;

/// Declaration identity used to decide Kotlin `conflicting overloads`.
///
/// This is not a backend descriptor key. Generic arguments, nullability, function shapes, and
/// value-class identities stay intact, and the type-parameter count stays even when a parameter
/// appears only in the result.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::resolve) struct TopLevelFunctionConflictKey {
    pub(in crate::resolve) package: TypeName,
    pub(in crate::resolve) receiver: Option<Ty>,
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) params: Vec<Ty>,
    pub(in crate::resolve) formal_bounds: Vec<(u32, Vec<Ty>)>,
    pub(in crate::resolve) type_parameter_count: u32,
}

impl TopLevelFunctionConflictKey {
    pub(in crate::resolve) fn from_signature(signature: &Signature, name: String) -> Option<Self> {
        let formals = signature
            .generic_sig
            .as_ref()
            .map(|generic| generic.formals.as_slice())
            .unwrap_or_default();
        let params = signature
            .generic_sig
            .as_ref()
            .map(|generic| generic.params.as_slice())
            .unwrap_or(&signature.params);
        if params.iter().any(|parameter| parameter.contains_error()) {
            return None;
        }
        // Canonicalize only declaration-owned type-parameter names so alpha-equivalent
        // declarations still conflict.
        let normalize = |ty| crate::types::ty_canonicalize_params(ty, formals);
        let declared_receiver = signature
            .generic_sig
            .as_ref()
            .and_then(|generic| generic.receiver)
            .or(signature.source_receiver);
        let formal_bounds = signature
            .generic_sig
            .as_ref()
            .map(|generic| {
                generic
                    .formal_bounds
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| {
                        // A formal that did not survive as a symbolic type parameter still counts
                        // toward arity below; it cannot be mentioned by a value parameter.
                        let Some(formal) = formals.get(*index) else {
                            return false;
                        };
                        params.iter().copied().chain(declared_receiver).any(|ty| {
                            crate::types::ty_mentions_param(ty, std::slice::from_ref(formal))
                        })
                    })
                    .map(|(index, bounds)| {
                        (
                            index as u32,
                            bounds.iter().copied().map(normalize).collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let receiver = match declared_receiver {
            Some(receiver) if receiver.contains_error() => return None,
            Some(receiver) => Some(normalize(receiver)),
            None => None,
        };
        // `formal_bounds` keeps one slot per generic-signature parameter. `formals` can be shorter
        // when a bound is not itself a type-parameter type. Callers that still have the declaration
        // header replace this count with that header's type-parameter list.
        let declared_type_parameters = signature
            .generic_sig
            .as_ref()
            .map(|generic| generic.formal_bounds.len().max(generic.formals.len()))
            .unwrap_or(0);
        let type_parameter_count = u32::try_from(declared_type_parameters).unwrap_or(u32::MAX);
        Some(Self {
            package: signature.package,
            receiver,
            name,
            params: params.iter().copied().map(normalize).collect(),
            formal_bounds,
            type_parameter_count,
        })
    }
}
