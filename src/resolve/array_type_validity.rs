//! Array type validity at finalized expression boundaries.
//!
//! Class literals own their feature-gated LHS policy. Ordinary values are checked independently,
//! after their selected call and reified substitutions have committed.

use super::*;
use crate::types::TyKind;

impl Checker<'_> {
    pub(super) fn check_array_of_nothing_expression(&mut self, expression: ExprId, ty: Ty) {
        if self.discover_anonymous_captures
            || self.suppresses_diagnostic("UNSUPPORTED")
            || matches!(
                self.expr_lowers.get(&expression),
                Some(ExprLowering::ClassLiteral { .. })
            )
            || !contains_array_of(ty, Ty::Nothing)
        {
            return;
        }
        self.diags.error(
            self.call_callee_name_span(expression),
            "expression cannot have a type of 'Array<Nothing>'.".to_string(),
        );
    }
}

/// Whether `ty` is `Array<element>` or one of its semantic type arguments contains one.
pub(super) fn contains_array_of(ty: Ty, element: Ty) -> bool {
    let ty = ty.non_null();
    if ty.is_reference_array()
        && ty.type_args().first().copied().and_then(argument_type) == Some(element)
    {
        return true;
    }
    ty.type_args()
        .iter()
        .filter_map(|argument| argument_type(*argument))
        .any(|argument| contains_array_of(argument, element))
}

/// The semantic type carried by an ordinary or projected type argument; stars name no type.
fn argument_type(argument: Ty) -> Option<Ty> {
    match argument.kind() {
        TyKind::StarProjection(_) => None,
        TyKind::InProjection(inner) | TyKind::OutProjection(inner) => Some(inner),
        _ => Some(argument),
    }
}
