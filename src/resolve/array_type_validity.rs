//! Array type validity at finalized expression boundaries.
//!
//! Class literals own their feature-gated LHS policy. Ordinary values are checked independently,
//! after their selected call and reified substitutions have committed. `Array<Nothing>` is
//! rejected everywhere; whether `Array<Nothing?>` is a value is the platform's answer.

use super::*;
use crate::types::TyKind;

impl Checker<'_> {
    /// The array operation selected at `expression`: a compiler-built array literal or size
    /// constructor, or a call whose selected declaration is one of the `kotlin` array factory
    /// functions (`arrayOf`, `intArrayOf`, `emptyArray`, …).
    pub(super) fn selected_array_operation(
        &self,
        expression: ExprId,
    ) -> Option<crate::synthetics::SyntheticKind> {
        if let Some(ExprLowering::CompilerSynthetic(kind)) = self.expr_lowers.get(&expression) {
            return Some(*kind);
        }
        match self.resolved_calls.get(&expression) {
            Some(ResolvedCall::TopLevel(call)) => match call.callable.compiler_intrinsic {
                Some(crate::libraries::CompilerIntrinsic::ArrayFactory(kind)) => Some(kind),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether `expression` is an array factory call that kotlinc reads as an array literal: one
    /// of `arrayOf`, `emptyArray` and the primitive `…ArrayOf` inside an annotation argument. A
    /// literal is no call, so the checks of a call's reified type arguments and of its
    /// `Array<Nothing>` result do not apply to it.
    pub(super) fn is_annotation_array_literal_call(&self, expression: ExprId) -> bool {
        use crate::types::ArrayFactoryKind;
        self.annotation_argument_depth != 0
            && matches!(
                self.selected_array_operation(expression),
                Some(
                    ArrayFactoryKind::ReferenceVararg
                        | ArrayFactoryKind::PrimitiveVararg(_)
                        | ArrayFactoryKind::EmptyReference
                )
            )
    }

    pub(super) fn check_array_of_nothing_expression(&mut self, expression: ExprId, ty: Ty) {
        if self.discover_anonymous_captures
            || self.is_annotation_array_literal_call(expression)
            || self.suppresses_diagnostic("UNSUPPORTED")
            || matches!(
                self.expr_lowers.get(&expression),
                Some(ExprLowering::ClassLiteral { .. })
            )
        {
            return;
        }
        let nullable = self.libraries.array_of_nullable_nothing_error();
        let messages = [
            (
                Ty::Nothing,
                Some("expression cannot have a type of 'Array<Nothing>'."),
            ),
            (Ty::nullable(Ty::Nothing), nullable),
        ];
        for (element, message) in messages {
            if let Some(message) = message.filter(|_| contains_array_of(ty, element)) {
                self.diags
                    .error(self.call_callee_name_span(expression), message.to_string());
            }
        }
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
