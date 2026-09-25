//! Value-class representation of checked call results.
//!
//! Providers attach the selected declaration and logical result to the call, and the JVM
//! `call_result_boundaries` pass records each relevant generic result-slot boundary and its physical
//! result before value-class lowering. This module consumes those facts in precedence order. It
//! neither looks up a callable nor infers semantics from an owner/member name.

use super::{desc, erase, repr_of_ty, CallTypes, Repr, Under};
use crate::ir::{Callee, ExprId};
use crate::types::Ty;
use std::collections::HashMap;

pub(super) fn representation(
    id: ExprId,
    callee: &Callee,
    under: &Under,
    types: CallTypes<'_>,
    physical: &HashMap<u32, Ty>,
) -> Repr {
    // A callee that returns a value class BY DECLARATION hands back its erased CARRIER: that is the
    // whole classpath value-class return ABI (`fun make(): K` -> `make-<hash>(): String`), whatever
    // the underlying erases to. This checked identity distinguishes `A.create(): A<String>` from
    // `List<TokenBox>.get`: both have an `Object` descriptor, but the latter declares bare `E`.
    if let Some(declared) = types.declared_value_class(id, under) {
        return Repr::Unboxed(declared);
    }
    let Some(logical) = types.get(&id) else {
        return Repr::NotVc;
    };
    let Some(value_class) = logical
        .non_null()
        .obj_internal()
        .filter(|classifier| under.contains_key(classifier))
    else {
        return Repr::NotVc;
    };

    // The carrier-aware JVM boundary records the erased slot for a declaration whose result is a
    // bare type parameter (`Iterator<X>.next`, `List<X>.get`). Once a value-class declaration itself
    // has been excluded above, that generic slot holds the BOX. Its descriptor may equal the
    // carrier's descriptor, so this recorded representation fact decides before comparison.
    if physical
        .get(&id)
        .is_some_and(|physical| physical.is_erased_top())
    {
        return Repr::Boxed(value_class);
    }

    let physical_descriptor = match callee {
        Callee::Virtual {
            params: Some((_, result)),
            ..
        } => Some(desc(result)),
        Callee::Static { descriptor, .. }
        | Callee::Virtual { descriptor, .. }
        | Callee::Special { descriptor, .. } => descriptor.rsplit(')').next().map(str::to_string),
        _ => None,
    };
    let carrier_descriptor = desc(&erase(&under[&value_class], under));
    if physical_descriptor.as_deref() == Some(carrier_descriptor.as_str()) {
        repr_of_ty(logical, under)
    } else {
        Repr::Boxed(value_class)
    }
}
