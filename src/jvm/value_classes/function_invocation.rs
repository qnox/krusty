//! A direct call of a value class that implements a function type.
//!
//! That call is the value class's selected `invoke` member. The member-call path realizes the
//! static implementation from that declaration. An `InvokeFunction` node still naming a non-null
//! value class has no implementation identity; emitting `FunctionN.invoke` would box the carrier.

use super::{ReprCtx, Under};
use crate::ir::{ExprId, IrExpr};
use crate::types::Ty;

/// Whether `expression` calls a non-null value class as a function value.
///
/// The checked call should have been the selected member. There is no name or arity to search:
/// a missing implementation is not `FunctionN.invoke`.
pub(super) fn missing_member_implementation(
    logical_types: &std::collections::HashMap<ExprId, Ty>,
    suspend_calls: &std::collections::HashMap<ExprId, Ty>,
    under: &Under,
    repr_ctx: &ReprCtx<'_>,
    id: ExprId,
    expression: &IrExpr,
) -> bool {
    let IrExpr::InvokeFunction { func, .. } = expression else {
        return false;
    };
    if suspend_calls.contains_key(&id) {
        return false;
    }
    value_class_callee(logical_types, repr_ctx, under, *func).is_some()
}

fn value_class_callee(
    logical_types: &std::collections::HashMap<ExprId, Ty>,
    repr_ctx: &ReprCtx<'_>,
    under: &Under,
    func: ExprId,
) -> Option<crate::types::TypeName> {
    if let Some(ty) = logical_types.get(&func).copied() {
        if ty.is_nullable() {
            return None;
        }
        let owner = ty.non_null().obj_internal()?;
        return under.contains_key(&owner).then_some(owner);
    }
    match repr_ctx.repr(func) {
        super::Repr::Unboxed(owner) => Some(owner),
        super::Repr::Boxed(_) | super::Repr::NotVc => None,
    }
}
