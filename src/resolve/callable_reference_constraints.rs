//! Constraints a selected callable reference contributes to an enclosing postponed call.
//!
//! Adaptation realizes a reference in the shape its context expects: `::add` against
//! `(String) -> Boolean` becomes a `(String) -> Boolean` value. When the selected callable's own
//! inputs or result mention a variable of an enclosing postponed call, as `MutableList<E>.add`
//! does inside `buildList { … }`, the realized shape no longer mentions it. The relation between
//! the two shapes is published here, on the ordinary commit path, as it is for a call's argument:
//! each supplied input flows into the callable's parameter, and its result flows into the expected
//! one (`String <: E`).

use super::callable_reference_selection::AdaptedRefArgument;
use super::*;

impl Checker<'_> {
    /// Publish the selected reference `expression`'s relations to the shape it was realized in.
    pub(super) fn publish_callable_reference_constraints(&mut self, expression: ExprId) {
        let Some((natural_params, natural_ret, plan, realized)) =
            self.adapted_reference_shapes(expression)
        else {
            return;
        };
        let TyKind::Fun(realized) = realized.kind() else {
            return;
        };
        let span = self.span(expression);
        for (slot, argument) in plan.iter().enumerate() {
            let Some(&parameter) = natural_params.get(slot) else {
                continue;
            };
            let (values, parameter) = match argument {
                AdaptedRefArgument::Value(value) => (std::slice::from_ref(value), parameter),
                AdaptedRefArgument::Vararg {
                    values,
                    whole_array,
                    ..
                } => (
                    values.as_slice(),
                    super::callable_reference_selection::vararg_input_type(parameter, *whole_array),
                ),
                AdaptedRefArgument::Context | AdaptedRefArgument::Default => continue,
            };
            for &value in values {
                let Some(&input) = realized.params.get(value) else {
                    continue;
                };
                if self.postponed_call_mentions(parameter) {
                    self.expect_assignable(parameter, input, span, "callable reference input");
                }
            }
        }
        if realized.ret != Ty::Unit && self.postponed_call_mentions(natural_ret) {
            self.expect_assignable(realized.ret, natural_ret, span, "callable reference result");
        }
    }

    /// The selected callable's natural inputs and result, the plan from the realized inputs to
    /// them, and the realized function type, for a reference realized through adaptation.
    fn adapted_reference_shapes(
        &self,
        expression: ExprId,
    ) -> Option<(Vec<Ty>, Ty, Vec<AdaptedRefArgument>, Ty)> {
        match self.expr_lowers.get(&expression)? {
            ExprLowering::AdaptedCallableReference {
                binding,
                target,
                argument_mapping,
                signature,
            } => {
                let (params, ret) = match target {
                    CallableReferenceTarget::Member { member, .. }
                    | CallableReferenceTarget::Classifier(member) => {
                        (member.params.clone(), member.ret)
                    }
                    _ => return None,
                };
                let receiver = match (binding, target) {
                    (
                        CallableReferenceBinding::Unbound,
                        CallableReferenceTarget::Member { receiver, .. },
                    ) => Some(*receiver),
                    _ => None,
                };
                let (natural_params, plan) = match receiver {
                    Some(receiver) => (
                        std::iter::once(receiver).chain(params).collect(),
                        std::iter::once(AdaptedRefArgument::Value(0))
                            .chain(argument_mapping.iter().cloned())
                            .collect(),
                    ),
                    None => (params, argument_mapping.clone()),
                };
                Some((natural_params, ret, plan, *signature))
            }
            ExprLowering::AdaptedRef {
                target,
                adapted_params,
                ret,
                resolved_argument_mapping,
                ..
            } => Some((
                target.params.clone(),
                target.ret,
                resolved_argument_mapping.clone(),
                Ty::fun(adapted_params.clone(), *ret),
            )),
            _ => None,
        }
    }
}
