//! Class-literal diagnostics for arrays whose element type is `Nothing`.
//!
//! The common `FirArrayOfNothingClassLiteralChecker` reports `Array<Nothing>` and the JVM
//! `FirArrayOfNullableNothingClassLiteralChecker` reports `Array<Nothing?>`. Both consume the
//! class-literal LHS's already-resolved semantic type; this checker never repeats name lookup.

use super::*;
use crate::types::TyKind;

impl Checker<'_> {
    /// Check the class literal whose left-hand side `lhs` resolved to classifier type `ty`.
    pub(super) fn check_array_of_nothing_class_literal(&mut self, lhs: ExprId, ty: Ty) {
        let messages = [
            (
                Ty::Nothing,
                "expression cannot have a type of 'Array<Nothing>'.",
            ),
            (
                Ty::nullable(Ty::Nothing),
                "'Array<Nothing?>' is not supported on the JVM.",
            ),
        ];
        for (element, message) in messages {
            if !contains_array_of(ty, element) {
                continue;
            }
            let span = self.span(lhs);
            if self.file.forbid_array_of_nothing_in_lhs_of_class_literal {
                self.diags.error(span, message.to_string());
            } else if !self
                .suppresses_diagnostic("UNSUPPORTED_ARRAY_OF_NOTHING_IN_CLASS_LITERAL_LHS")
            {
                self.diags.warning(span, message.to_string());
            }
        }
    }
}

/// Whether `ty` is `Array<element>` or one of its semantic type arguments contains one.
fn contains_array_of(ty: Ty, element: Ty) -> bool {
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
