//! Kotlin's conversions of an already-materialized function value to another function type.
//!
//! A value of a regular function type converts to a `suspend` function type of the same shape
//! (suspend conversion), and under `+UnitConversionsOnArbitraryExpressions` a value whose result is
//! not `Unit` converts to the same shape returning `Unit` (unit conversion). The two compose, and a
//! value that is already `suspend` unit-converts to a `suspend` target. The resolver selects the
//! exact callable constituent the conversion wraps; checked FIR carries the pair and common
//! lowering realizes it without selecting again.
//!
//! A function value's applicability to a function-typed parameter lives here too, because the
//! argument boundary is where unit conversion may apply to the value's own result.
//!
//! kotlinc converts a value only where it is passed as a call argument (positional, named, or a
//! conditional passed as one). An initializer, assignment, return, expression body or lambda
//! result of a regular function type is a type mismatch against a `suspend` or `Unit`-returning
//! target, so only the argument seams consult the conversion.

use crate::ast::{Expr, ExprId};
use crate::types::{FnSig, Ty, TyKind};

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
                    // A `null` value stays `null`, unless the release converts it to an adapter
                    // that yields `null`.
                    let keeps_null = source.admits_null()
                        && !crate::kotlin_version::Quirks::target()
                            .has(crate::kotlin_version::Quirk::NullFunctionValueConvertsToAdapter);
                    self.selected_function_value_conversions
                        .insert(argument, (source, expected, keeps_null));
                }
                source
            }
            None => self.recorded_expression_type_for_expected(scope, argument, nominal, expected),
        }
    }

    /// Whether a function type returning `expected` admits a value whose result is not `Unit`
    /// through unit conversion. Some releases know the feature but convert no arbitrary value
    /// ([`crate::kotlin_version::Quirk::UnitConversionRejectsNonUnitResult`]).
    pub(super) fn unit_conversion_admits_result(&self, expected: Ty) -> bool {
        self.file.unit_conversions_on_arbitrary_expressions
            && !self
                .file
                .reference
                .quirks()
                .has(crate::kotlin_version::Quirk::UnitConversionRejectsNonUnitResult)
            && expected == Ty::Unit
    }

    /// Whether the function value `actual` fits a function-typed parameter `expected` whose
    /// signature may have reached the checker erased: a classpath `(StringBuilder) -> Unit` is a bare
    /// `Function1`, `(Any) -> Any`, and a source `() -> T` erases the same way. The two match on
    /// arity and suspend-ness; parameters are accepted contravariantly and the result covariantly,
    /// and a component that is the erased top or mentions a type parameter is left to inference.
    pub(super) fn erased_function_param_fits(&self, expected: Ty, actual: Ty) -> bool {
        self.function_shape_fits(expected, actual, true)
    }

    /// [`Self::erased_function_param_fits`] for a generic constructor's function-typed parameter,
    /// where either side may be nullable.
    pub(super) fn generic_function_constructor_arg_fits(&self, expected: Ty, actual: Ty) -> bool {
        if actual == Ty::Null {
            return expected.is_nullable();
        }
        if actual.is_nullable() && !expected.is_nullable() {
            return false;
        }
        self.erased_function_param_fits(expected.non_null(), actual.non_null())
    }

    /// The shape comparison. Unit conversion of the value's own result applies only `at_argument`:
    /// it is an argument-boundary conversion, so a function type nested in a parameter or result
    /// compares by subtyping alone (`(() -> Unit) -> Unit` does not reach `(() -> String) -> Unit`).
    fn function_shape_fits(&self, expected: Ty, actual: Ty, at_argument: bool) -> bool {
        let (TyKind::Fun(expected), TyKind::Fun(actual)) = (expected.kind(), actual.kind()) else {
            return false;
        };
        expected.params.len() == actual.params.len()
            // Suspend conversion is one-way: a regular function value may be wrapped as suspend;
            // a suspend value cannot flow to a regular function parameter.
            && (!actual.suspend || expected.suspend)
            && expected
                .params
                .iter()
                .zip(&actual.params)
                .all(|(expected, actual)| self.function_component_accepts(*actual, *expected))
            // Lambdas and callable references are normalized before this comparison. A stored
            // `() -> String` value reaches `() -> Unit` only through the language's unit conversion.
            && (self.function_component_accepts(expected.ret, actual.ret)
                || at_argument && self.unit_conversion_admits_result(expected.ret))
    }

    /// Whether one component of a function type, `supertype`, accepts `subtype`. A component typed
    /// by the `null` literal is `Nothing?`. A nested function type compares by shape again, so its own
    /// erased or generic components stay open to inference.
    fn function_component_accepts(&self, supertype: Ty, subtype: Ty) -> bool {
        let supertype = if supertype == Ty::Null {
            Ty::nullable(Ty::Nothing)
        } else {
            supertype
        };
        if supertype == subtype
            || supertype.is_erased_top()
            || supertype.mentions_ty_param()
            || subtype.mentions_ty_param()
            || self.postponed_call_mentions(supertype)
            || self.postponed_call_mentions(subtype)
        {
            return true;
        }
        if subtype == Ty::Null {
            return supertype.is_nullable();
        }
        crate::assignable::is_assignable(&crate::assignable::TyCtx::new(), self, subtype, supertype)
            || (!subtype.is_nullable() || supertype.is_nullable())
                && self.function_shape_fits(supertype.non_null(), subtype.non_null(), false)
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
            && expected.non_null().is_fun()
            && actual.non_null().is_fun()
            && self.member_argument_score(expected, actual).is_none()
    }

    /// The type a rejected function value is reported as: suspend conversion applies before the
    /// rest of the value's shape is compared, so a regular value reaching a suspend parameter is
    /// reported as the suspend function it converted to.
    pub(super) fn suspend_converted_argument(&self, expected: Ty, actual: Ty) -> Ty {
        match (expected.non_null().kind(), actual.kind()) {
            (TyKind::Fun(target), TyKind::Fun(source)) if target.suspend && !source.suspend => {
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
        let TyKind::Fun(target) = expected.non_null().kind() else {
            return None;
        };
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
                let TyKind::Fun(source) = candidate.non_null().kind() else {
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
                // A nullable value converts only where the target admits `null` too.
                let converted_target = expected.rewrap_nullability(Ty::fun_with_shape(
                    target.params.clone(),
                    if converts_result {
                        source.ret
                    } else {
                        target.ret
                    },
                    target.context_count,
                    target.has_receiver,
                    source.suspend,
                ));
                self.receiver_is_assignable(*candidate, converted_target)
            })
            .map(|candidate| {
                // A type parameter whose bound admits `null` can hold `null`, which the converted
                // value then stays. Releases with `NullFunctionValueConvertsToAdapter` convert the
                // value as if it were present.
                if nominal.upper_bound_admits_null()
                    && !candidate.admits_null()
                    && !crate::kotlin_version::Quirks::target()
                        .has(crate::kotlin_version::Quirk::NullFunctionValueConvertsToAdapter)
                {
                    Ty::nullable(candidate)
                } else {
                    candidate
                }
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
