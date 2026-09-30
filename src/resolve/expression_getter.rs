//! Expression-bodied property getters.
//!
//! An explicit getter result is a declaration boundary: a platform value committed there is guarded
//! inside the getter. An inferred getter keeps the platform result, so the guard belongs at a later
//! non-null use.

use crate::ast::ExprId;
use crate::types::Ty;

use super::{Checker, CheckerScope, PlatformNarrowing};

impl<'a> Checker<'a> {
    pub(super) fn check_expression_getter(
        &mut self,
        scope: &CheckerScope<'_>,
        body: ExprId,
        result: Ty,
        declared: bool,
    ) {
        let actual = if declared {
            let checked = self.expr_declared(scope, body, result);
            let actual = self.recorded_expression_type_for_expected(scope, body, checked, result);
            self.narrow_platform_value(result, body, PlatformNarrowing::Declaration);
            actual
        } else {
            self.expr_expected(scope, body, result)
        };
        self.expect_assignable(result, actual, self.span(body), "getter body");
    }
}
