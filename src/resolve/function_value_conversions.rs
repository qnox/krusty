//! Kotlin's conversions of an already-materialized function value to another function type.
//!
//! A value of a regular function type converts to a `suspend` function type of the same shape
//! (suspend conversion), and under `+UnitConversionsOnArbitraryExpressions` a value whose result is
//! not `Unit` converts to the same shape returning `Unit` (unit conversion). The two compose, and a
//! value that is already `suspend` unit-converts to a `suspend` target. The resolver selects the
//! exact callable constituent the conversion wraps; checked FIR carries the pair and common
//! lowering realizes it without selecting again.
//!
//! kotlinc converts a value only where it is passed as a call argument (positional, named, or a
//! conditional passed as one). An initializer, assignment, return, expression body or lambda
//! result of a regular function type is a type mismatch against a `suspend` or `Unit`-returning
//! target, so only the argument seams consult the conversion.

use crate::ast::{Expr, ExprId};
use crate::types::{FnSig, Ty};

use super::{Checker, CheckerScope};

impl Checker<'_> {
    /// [`Checker::expression_type_for_expected`] at a call-argument boundary, where a function
    /// value also reaches `expected` through a function-value conversion.
    pub(super) fn argument_type_for_expected(
        &self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        nominal: Ty,
        expected: Ty,
    ) -> Ty {
        self.function_value_conversion_source(scope, argument, nominal, expected)
            .unwrap_or_else(|| {
                self.expression_type_for_expected(scope, argument, nominal, expected)
            })
    }

    /// Commit the type of a selected call's argument. Reaching this seam means the call has
    /// committed its parameter type, so the exact callable constituent a function-value conversion
    /// wraps is retained for checked FIR. A callable reference carries its own checked adaptation
    /// and never acquires a second wrapper.
    pub(super) fn recorded_argument_type_for_expected(
        &mut self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        nominal: Ty,
        expected: Ty,
    ) -> Ty {
        self.selected_function_value_conversions.remove(&argument);
        match self.function_value_conversion_source(scope, argument, nominal, expected) {
            Some(source) => {
                if !matches!(self.file.expr(argument), Expr::CallableRef { .. }) {
                    self.selected_function_value_conversions
                        .insert(argument, (source, expected));
                }
                source
            }
            None => self.recorded_expression_type_for_expected(scope, argument, nominal, expected),
        }
    }

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

    /// Select the exact callable constituent of `expression` that a function-value conversion
    /// wraps for `expected`. As in kotlinc, kind conversion applies first and unit conversion then
    /// compares the rest, so an already-suspend constituent is kept for a suspend target and only
    /// its result converts. The nominal value can implement several function supertypes; arity
    /// alone is not enough, so each complete function type is compared against the target with the
    /// converted components (suspension, a non-`Unit` result) taken from the candidate. `None`
    /// when the value needs no conversion or none applies.
    fn function_value_conversion_source(
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
                // Kind conversion only adds suspension: a suspend value never reaches a regular
                // function type. An already-suspend value keeps its kind for a suspend target.
                if source.suspend && !target.suspend {
                    return false;
                }
                let converts_result =
                    unit_conversion && !self.receiver_is_assignable(source.ret, Ty::Unit);
                if source.suspend == target.suspend && !converts_result {
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
                    source.suspend,
                );
                self.receiver_is_assignable(*candidate, converted_target)
            })
    }

    /// Whether a function value of shape `actual` reaches `expected` at a `context` boundary only
    /// through a function-value conversion, which only a call argument admits: a regular value
    /// where a `suspend` function is expected (or the reverse, which never converts), or a value
    /// with a concrete non-`Unit` result where the target returns `Unit`.
    pub(super) fn function_value_needs_conversion(
        &self,
        context: &str,
        expected: &FnSig,
        actual: &FnSig,
    ) -> bool {
        if matches!(context, "argument" | "generic argument") {
            return false;
        }
        let converts_result = expected.ret == Ty::Unit
            && !matches!(actual.ret, Ty::Unit | Ty::Nothing | Ty::Pending | Ty::Error)
            && !actual.ret.is_erased_top()
            && !actual.ret.mentions_ty_param()
            && !actual.ret.mentions_pending();
        expected.suspend != actual.suspend || converts_result
    }
}
