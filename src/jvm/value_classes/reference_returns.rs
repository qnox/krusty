//! A function declared to return a reference supertype of a value class (`Any`, `Any?`, or an
//! interface a value class implements) hands each returned value to its caller through a reference
//! slot. That return is the same representation boundary as an `Any` parameter or local: kotlinc
//! coerces the returned value from its own representation to the declared return type, so a
//! carrier is boxed and a value that already is the box (a generic call substituted with the value
//! class) is returned as it is.

use super::{record_value_boundary, BoxOp, CarrierUnboxes, FieldGetters, ReprInputs, Under};
use crate::backend::BackendClassifierSource;
use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};
use crate::types::{Ty, TypeName};
use std::collections::{HashMap, HashSet};

/// The declarations whose returns cross this boundary, with the pre-erasure facts the
/// representation of each returned value is read from.
pub(super) struct ReferenceReturns<'a> {
    pub(super) rets: &'a [Ty],
    pub(super) fields: &'a [Vec<Ty>],
    pub(super) slot_types: &'a [HashMap<u32, Ty>],
    pub(super) under: &'a Under,
    pub(super) field_getters: &'a FieldGetters,
    pub(super) carrier_unboxes: &'a CarrierUnboxes,
    /// Every reference supertype of a known value class, including indirect interfaces.
    pub(super) value_class_reference_supertypes: &'a HashSet<TypeName>,
    /// Value-class members; those not lowered to static carrier functions are synthesized wrappers
    /// whose bodies run on the box and keep their own returns.
    pub(super) value_members: &'a HashSet<u32>,
    /// The value-class members realized as statics over the carrier.
    pub(super) static_members: &'a HashSet<u32>,
}

/// The transitive semantic supertypes of every value class known to this compilation. The backend
/// handoff already froze direct classifier relationships, so this is an identity traversal over
/// the common class model, not a provider lookup or a reconstruction from JVM names.
pub(super) fn value_class_reference_supertypes(
    classifiers: &dyn BackendClassifierSource,
    value_classes: &Under,
) -> HashSet<TypeName> {
    let mut pending = value_classes
        .keys()
        .filter_map(|classifier| classifiers.classifier(*classifier))
        .flat_map(|facts| facts.supertypes.iter().copied().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let mut supertypes = HashSet::new();
    while let Some(classifier) = pending.pop() {
        if !supertypes.insert(classifier) {
            continue;
        }
        if let Some(facts) = classifiers.classifier(classifier) {
            pending.extend(facts.supertypes.iter().copied());
        }
    }
    supertypes
}

/// Record, for every function declared to return `Any` or a value-class interface, the boundary
/// from each value it returns to that declared result. A lambda implementation declared so is one
/// of them: its erased `invoke` result is the same reference slot.
pub(super) fn record_reference_returns(
    ops: &mut Vec<(ExprId, BoxOp)>,
    ir: &IrFile,
    returns: &ReferenceReturns<'_>,
) {
    for (fid, function) in ir.functions.iter().enumerate() {
        let result = returns.rets[fid];
        let reference_supertype = result.non_null().obj_internal().is_some_and(|classifier| {
            classifier == crate::types::wk::any()
                || returns
                    .value_class_reference_supertypes
                    .contains(&classifier)
        });
        let (Some(body), true) = (function.body, reference_supertype) else {
            continue;
        };
        if returns.value_members.contains(&(fid as u32))
            && !returns.static_members.contains(&(fid as u32))
        {
            continue;
        }
        let repr_ctx = ReprInputs {
            rets: returns.rets,
            fields: returns.fields,
            slots: &returns.slot_types[fid],
            under: returns.under,
            field_getters: returns.field_getters,
            carrier_unboxes: returns.carrier_unboxes,
        }
        .over(ir);
        let mut returned = Vec::new();
        collect_returned_values(&ir.exprs, body, &mut returned);
        for value in returned {
            record_value_boundary(ops, &ir.exprs, &repr_ctx, value, result, returns.under);
        }
    }
}

/// Every value a `return` of this function hands back, a guard clause as much as the tail. A
/// lambda's returns belong to the lambda; only its captures are evaluated here.
fn collect_returned_values(exprs: &[IrExpr], id: ExprId, out: &mut Vec<ExprId>) {
    match &exprs[id as usize] {
        IrExpr::Return(Some(value)) => {
            collect_returned_values(exprs, *value, out);
            out.push(*value);
        }
        IrExpr::Lambda { captures, .. } => {
            for &capture in captures {
                collect_returned_values(exprs, capture, out);
            }
        }
        _ => for_each_child(exprs, id, &mut |child| {
            collect_returned_values(exprs, child, out)
        }),
    }
}
