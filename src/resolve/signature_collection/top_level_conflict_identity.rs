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
    package: TypeName,
    receiver: Option<Ty>,
    name: String,
    params: Vec<Ty>,
    /// A `vararg` parameter is not its array: `f(vararg x: Int)` and `f(x: IntArray)` coexist.
    vararg_index: Option<usize>,
    formal_bounds: Vec<(u32, Vec<Ty>)>,
    type_parameter_count: usize,
    /// A `@LowPriorityInOverloadResolution` declaration conflicts only with another one: it yields
    /// to an ordinary declaration of its shape instead of clashing with it.
    low_priority: bool,
}

impl TopLevelFunctionConflictKey {
    pub(in crate::resolve) fn from_signature(
        signature: &Signature,
        name: String,
        declared_type_parameter_count: usize,
    ) -> Option<Self> {
        let declared_receiver = signature
            .generic_sig
            .as_ref()
            .and_then(|generic| generic.receiver)
            .or(signature.source_receiver);
        let shape = signature.generic_sig.as_ref().map_or(
            ConflictShape {
                formals: &[],
                formal_bounds: &[],
                params: &signature.params,
                vararg_index: signature.vararg_index,
                receiver: declared_receiver,
                annotations: &signature.annotations,
            },
            |generic| ConflictShape {
                formals: &generic.formals,
                formal_bounds: &generic.formal_bounds,
                params: &generic.params,
                vararg_index: signature.vararg_index,
                receiver: declared_receiver,
                annotations: &signature.annotations,
            },
        );
        Self::from_shape(
            signature.package,
            name,
            shape,
            declared_type_parameter_count,
        )
    }

    /// The overload shape of a constructor in its package scope: a callable named by its class or
    /// typealias whose formals are that declaration's own.
    pub(in crate::resolve) fn from_constructor(
        package: TypeName,
        name: String,
        signature: &crate::libraries::GenericSig,
        member: &crate::libraries::LibraryMember,
    ) -> Option<Self> {
        Self::from_shape(
            package,
            name,
            ConflictShape {
                formals: &signature.formals,
                formal_bounds: &signature.formal_bounds,
                params: &signature.params,
                vararg_index: member.call_sig.vararg_index,
                receiver: None,
                annotations: &member.annotations,
            },
            signature.formals.len(),
        )
    }

    fn from_shape(
        package: TypeName,
        name: String,
        shape: ConflictShape<'_>,
        declared_type_parameter_count: usize,
    ) -> Option<Self> {
        let ConflictShape {
            formals,
            formal_bounds,
            params,
            vararg_index,
            receiver: declared_receiver,
            annotations,
        } = shape;
        if params.iter().any(|parameter| parameter.contains_error()) {
            return None;
        }
        // Canonicalize only declaration-owned type-parameter names so alpha-equivalent
        // declarations still conflict.
        let normalize = |ty| crate::types::ty_canonicalize_params(ty, formals);
        let formal_bounds = formal_bounds
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                // A formal that did not survive as a symbolic type parameter still counts toward
                // arity below; it cannot be mentioned by a value parameter.
                let Some(formal) = formals.get(*index) else {
                    return false;
                };
                params
                    .iter()
                    .copied()
                    .chain(declared_receiver)
                    .any(|ty| crate::types::ty_mentions_param(ty, std::slice::from_ref(formal)))
            })
            .map(|(index, bounds)| {
                (
                    index as u32,
                    bounds.iter().copied().map(normalize).collect(),
                )
            })
            .collect();
        let receiver = match declared_receiver {
            Some(receiver) if receiver.contains_error() => return None,
            Some(receiver) => Some(normalize(receiver)),
            None => None,
        };
        Some(Self {
            package,
            receiver,
            name,
            params: params.iter().copied().map(normalize).collect(),
            vararg_index,
            formal_bounds,
            type_parameter_count: declared_type_parameter_count,
            low_priority:
                crate::resolve::scope_constructor_candidates::is_low_priority_in_overload_resolution(
                    annotations,
                ),
        })
    }
}

/// The parts of a callable signature that decide its overload identity.
struct ConflictShape<'a> {
    formals: &'a [crate::types::TyParamName],
    formal_bounds: &'a [Vec<Ty>],
    params: &'a [Ty],
    vararg_index: Option<usize>,
    receiver: Option<Ty>,
    annotations: &'a [TypeName],
}
