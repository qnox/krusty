//! The JVM merge type of every value `when` once value classes are realized as their carriers.

use super::{erase, Under};
use crate::ir::{ExprId, IrExpr, IrFile};
use crate::jvm::physical_type::ir_ty_to_jvm;

/// The emitter merges a value `when`'s branches on the verifier-visible type recorded for it, not
/// on Kotlin's checked type. Once a value class has been realized as its carrier the merge frame
/// must use that same carrier (`SampleId?` over a non-null `String` field is `String`, including
/// its null branch); a semantic classifier there above erased branch values produces a
/// StackMapTable that claims `LSampleId;` above an actual `String`.
///
/// An exhaustive `when` result is itself that merge type, so it is erased in place. A value
/// `when` that records only its checked (logical) result, such as an elvis, keeps that semantic
/// type and gets the JVM type of its erased merge as this backend's physical fact.
pub(super) fn erase_when_merges(ir: &mut IrFile, under: &Under) {
    for result in ir.whens.exhaustive.values_mut() {
        *result = erase(result, under);
    }
    let erased_merges = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(id, expression)| {
            let id = ExprId::try_from(id).expect("too many expressions");
            let IrExpr::When { branches } = expression else {
                return None;
            };
            if !branches.iter().any(|(condition, _)| condition.is_none())
                || ir.whens.exhaustive.contains_key(&id)
                || ir.physical_type(id).is_some()
            {
                return None;
            }
            let logical = ir.logical_type(id)?;
            let erased = erase(&logical, under);
            (erased != logical).then(|| (id, ir_ty_to_jvm(&erased)))
        })
        .collect::<Vec<_>>();
    for (when, merge) in erased_merges {
        ir.set_physical_type(when, merge);
    }
}
