//! `value in start..end` as a KLIB serializes it: the range's `contains` called on the range
//! operator's own call.
//!
//! kotlinc's `RangeContainsLowering` compares the value with the bounds of a range literal instead
//! of building the range and calling its `contains`, and checked FIR does the same for the source
//! form. This module owns the serialized form both dependency preparation and body lowering read,
//! so the declaration edge preparation omits is exactly the call lowering realizes as the
//! comparison. Whether the operator and `contains` are the declarations the comparison stands for
//! is not decided here: body lowering authenticates both and declines a form they do not prove.

use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrExpr, KlibIrExprId, KlibIrExprKind, KlibIrMemberAccess,
};
use crate::metadata::klib_ir::KlibIrSignature;

/// A `contains` call the producer serialized for an `in` or `!in` over a range literal.
pub(super) struct RangeLiteralMembership<'a> {
    /// The `contains` call.
    pub(super) membership: &'a KlibIrMemberAccess,
    /// The range operator's call, `contains`' dispatch receiver.
    pub(super) range: &'a KlibIrMemberAccess,
    pub(super) value: KlibIrExprId,
}

/// `expression` as an `in`/`!in` over a range literal, structurally: the producer's own operator
/// origins on a `contains` call whose dispatch receiver is the range operator's call. A `!in`'s
/// negation is the enclosing `not`, which keeps the same origin.
pub(super) fn range_literal_membership<'a>(
    arena: &'a KlibIrArena,
    expression: &'a KlibIrExpr,
) -> Option<RangeLiteralMembership<'a>> {
    let KlibIrExprKind::Call {
        access: membership,
        super_qualifier: None,
    } = &expression.kind
    else {
        return None;
    };
    if !matches!(membership.origin.as_deref(), Some("IN" | "NOT_IN"))
        || !matches!(membership.symbol.signature, KlibIrSignature::Public(_))
        || !membership.type_arguments.is_empty()
    {
        return None;
    }
    let (receiver, value) = match &membership.arguments {
        KlibIrArguments::Flat(arguments) => match arguments.as_slice() {
            &[Some(receiver), Some(value)] => (receiver, value),
            _ => return None,
        },
        KlibIrArguments::Split {
            dispatch_receiver: Some(receiver),
            extension_receiver: None,
            values,
        } => match values.as_slice() {
            &[Some(value)] => (*receiver, value),
            _ => return None,
        },
        KlibIrArguments::Split { .. } => return None,
    };
    let KlibIrExprKind::Call {
        access: range,
        super_qualifier: None,
    } = &arena.expr(receiver).kind
    else {
        return None;
    };
    (matches!(range.origin.as_deref(), Some("RANGE" | "RANGE_UNTIL"))
        && matches!(range.symbol.signature, KlibIrSignature::Public(_))
        && range.type_arguments.is_empty())
    .then_some(RangeLiteralMembership {
        membership,
        range,
        value,
    })
}

/// Whether dependency preparation omits `expression`'s declaration edge, because body lowering
/// realizes it as a comparison or declines it.
pub(crate) fn folds_range_literal_membership(arena: &KlibIrArena, expression: &KlibIrExpr) -> bool {
    range_literal_membership(arena, expression).is_some()
}
