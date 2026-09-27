//! Kotlin's conversions of an already-materialized function value to another function type.
//!
//! A value of a regular function type converts to a `suspend` function type of the same shape
//! (suspend conversion), and under `+UnitConversionsOnArbitraryExpressions` a value whose result is
//! not `Unit` converts to the same shape returning `Unit` (unit conversion). The two compose. The
//! resolver selects the exact callable constituent the conversion wraps; checked FIR carries the
//! pair and common lowering realizes it without selecting again.

use crate::ast::{Expr, ExprId};
use crate::types::Ty;

use super::{Checker, CheckerScope};

impl Checker<'_> {
    /// Whether a function type returning `expected` admits a value whose result is not `Unit`
    /// through unit conversion. Earlier releases know the feature but convert no arbitrary value
    /// (KT-84393, fixed in 2.4.20).
    pub(super) fn unit_conversion_admits_result(&self, expected: Ty) -> bool {
        self.file.unit_conversions_on_arbitrary_expressions
            && crate::kotlin_version::at_least(crate::kotlin_version::KotlinVersion::V2_4_20)
            && expected == Ty::Unit
    }

    /// Whether the function value `argument` is inapplicable to a function-typed parameter. The
    /// assignability report compares function types by arity alone, so an inapplicable candidate's
    /// diagnostic consults the applicability relation, including any function-value conversion. A
    /// callable reference is not a value here: its own adaptation owns its diagnostics.
    pub(super) fn function_value_rejected(
        &self,
        argument: ExprId,
        expected: Ty,
        actual: Ty,
    ) -> bool {
        !matches!(self.file.expr(argument), Expr::CallableRef { .. })
            && matches!(expected.non_null(), Ty::Fun(_))
            && matches!(actual.non_null(), Ty::Fun(_))
            && self.member_argument_score(expected, actual).is_none()
    }

    /// The type a rejected function value is reported as: suspend conversion applies before the
    /// rest of the value's shape is compared, so a regular value reaching a suspend parameter is
    /// reported as the suspend function it converted to.
    pub(super) fn suspend_converted_argument(&self, expected: Ty, actual: Ty) -> Ty {
        match (expected.non_null(), actual) {
            (Ty::Fun(target), Ty::Fun(source)) if target.suspend && !source.suspend => {
                Ty::fun_with_shape(
                    source.params.clone(),
                    source.ret,
                    source.context_count,
                    source.has_receiver,
                    true,
                )
            }
            _ => actual,
        }
    }

    /// Select the exact regular callable constituent of `expression` that a function-value
    /// conversion wraps for `expected`. The nominal value can implement several function
    /// supertypes; arity alone is not enough, so each complete function type is compared against
    /// the target with the converted components (suspension, a non-`Unit` result) taken from the
    /// candidate. `None` when the value needs no conversion or none applies.
    pub(super) fn function_value_conversion_source(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        nominal: Ty,
        expected: Ty,
    ) -> Option<Ty> {
        let Ty::Fun(target) = expected.non_null() else {
            return None;
        };
        if nominal.is_nullable() {
            return None;
        }
        // A callable reference adapts its own result to `Unit` as part of its checked reference
        // adaptation; it is not a value converted after the fact.
        let unit_conversion = self.unit_conversion_admits_result(target.ret)
            && !matches!(self.file.expr(expression), Expr::CallableRef { .. });
        if !target.suspend && !unit_conversion {
            return None;
        }
        self.expression_function_types(scope, expression, nominal)
            .into_iter()
            .find(|candidate| {
                let Ty::Fun(source) = candidate else {
                    return false;
                };
                if source.suspend {
                    return false;
                }
                let converts_result =
                    unit_conversion && !self.receiver_is_assignable(source.ret, Ty::Unit);
                if !target.suspend && !converts_result {
                    return false;
                }
                let converted_target = Ty::fun_with_shape(
                    target.params.clone(),
                    if converts_result {
                        source.ret
                    } else {
                        target.ret
                    },
                    target.context_count,
                    target.has_receiver,
                    false,
                );
                self.receiver_is_assignable(*candidate, converted_target)
            })
    }
}
