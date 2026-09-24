//! Value-class representation at checked call-argument boundaries.
//!
//! The selected declaration's semantic parameter vector is authoritative for every call shape.
//! Only calls without that checked fact may fall back to an already-realized JVM descriptor. This
//! keeps local, module, and dependency calls on one identity-backed path without dispatching on
//! callee origin or spelling.

use super::{
    desc, descriptor_parameters, erase, is_value_class_internal, record_value_boundary, BoxOp,
    Repr, ReprCtx, Under,
};
use crate::ir::{Callee, ExprId};
use crate::types::Ty;

pub(super) fn record_boundaries(
    callee: &Callee,
    arguments: &[ExprId],
    declared_parameters: Option<&[Ty]>,
    extension_receiver_is_reference_value_class: bool,
    under: &Under,
    context: &ReprCtx<'_>,
    operations: &mut Vec<(ExprId, BoxOp)>,
) {
    if let Some(parameters) = declared_parameters {
        for (&argument, &parameter) in arguments.iter().zip(parameters) {
            let (value, _) = context.through_erased_generic_coercion(argument);
            record_value_boundary(operations, context.exprs, context, value, parameter, under);
        }
        return;
    }

    let (owner, descriptor) = match callee {
        Callee::Virtual {
            owner, descriptor, ..
        }
        | Callee::Static {
            owner, descriptor, ..
        }
        | Callee::Special {
            owner, descriptor, ..
        } => (*owner, descriptor.as_str()),
        _ => return,
    };

    let references = descriptor_parameters::references(descriptor);
    let parameter_types = descriptor_parameters::types(descriptor);
    // A one-argument operation owned by a value class that converts its carrier descriptor to the
    // class descriptor is the representation adapter itself. Identify that backend ABI shape rather
    // than dispatching on its synthetic spelling; its carrier input must never be boxed recursively.
    let value_class_owned = is_value_class_internal(owner, under);
    let carrier_descriptor = under
        .get(&owner)
        .map(|carrier| desc(&erase(carrier, under)));
    let box_descriptor = desc(&Ty::obj_name(owner));
    let adapts_carrier_to_box = value_class_owned
        && carrier_descriptor
            .as_deref()
            .is_some_and(|carrier| parameter_types.len() == 1 && parameter_types[0] == carrier)
        && descriptor.rsplit(')').next() == Some(box_descriptor.as_str());
    if adapts_carrier_to_box {
        return;
    }

    for (index, &argument) in arguments.iter().enumerate() {
        // The first ordinary argument of a static extension facade is its receiver. Its selected
        // source-receiver fact owns that representation boundary and was applied by the caller.
        if extension_receiver_is_reference_value_class && index == 0 {
            continue;
        }
        let (value, representation) = context.through_erased_generic_coercion(argument);
        let Repr::Unboxed(value_class) = representation else {
            continue;
        };
        // A reference parameter boxes an unboxed value class unless it is exactly that class's own
        // concrete carrier. Object remains ambiguous at descriptor level and therefore boxes; a
        // checked declaration vector, when available, took the authoritative path above.
        let carrier_descriptor = under
            .get(&value_class)
            .map(|carrier| desc(&erase(carrier, under)));
        let own_carrier = parameter_types.get(index).map(String::as_str)
            == carrier_descriptor.as_deref()
            && carrier_descriptor.as_deref() != Some("Ljava/lang/Object;");
        if references.get(index).copied().unwrap_or(false) && !own_carrier {
            operations.push((value, context.box_op(value, value_class)));
        }
    }
}
