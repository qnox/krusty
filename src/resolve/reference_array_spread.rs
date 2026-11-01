//! The element a spread argument contributes to an `arrayOf(...)` call.
//!
//! A spread argument of `vararg elements: T` is any `Array<out T>`, so `arrayOf(*items)` accepts
//! an out-projected array and contributes its read element type. An explicit or contextual element
//! type bounds the spread array the same way; an `in`-projected expectation does not bound it.

use super::*;

impl Checker<'_> {
    /// Check the spread `argument` of an `arrayOf`-family call against `element_hint` (the call's
    /// explicit or contextual element type, possibly projected) and return the element type it
    /// contributes, or `Ty::Error` once a diagnostic explains why it contributes none.
    pub(super) fn reference_array_spread_element(
        &mut self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        element_hint: Option<Ty>,
    ) -> Ty {
        let spread_array = |element| Ty::obj_args("kotlin/Array", &[Ty::out_projection(element)]);
        let expected = element_hint
            .filter(|hint| !matches!(hint.kind(), TyKind::InProjection(_)))
            .map(|hint| spread_array(hint.projection_read_ty()));
        let actual = match expected {
            Some(expected) => self.expr_expected(scope, argument, expected),
            None => self.expr(scope, argument),
        };
        let Some(contributed) = actual.non_null().array_read_elem() else {
            if actual != Ty::Error {
                self.diags.error(
                    self.span(argument),
                    "spread argument must have an array type".to_string(),
                );
            }
            return Ty::Error;
        };
        self.expect_assignable(
            spread_array(contributed),
            actual,
            self.span(argument),
            "array spread",
        );
        contributed
    }
}
