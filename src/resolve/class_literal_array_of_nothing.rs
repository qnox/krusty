//! Class-literal diagnostics for arrays whose element type is `Nothing`.
//!
//! The common `FirArrayOfNothingClassLiteralChecker` reports `Array<Nothing>` and the JVM
//! `FirArrayOfNullableNothingClassLiteralChecker` reports `Array<Nothing?>`. Both consume the
//! class-literal LHS's already-resolved semantic type; this checker never repeats name lookup.

use super::array_type_validity::contains_array_of;
use super::*;

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
