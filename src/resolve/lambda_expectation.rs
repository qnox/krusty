use crate::libraries::{CallSig, SemanticPlatform};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

use super::{
    call_argument_parameter_indices, generic_member_lambda_params, module_member_lambda_params,
    ContextualCallShape, GenericMemberPlan,
};

/// Contextual lambda shapes supplied by one selected module member, in source-argument order.
pub(super) struct MemberLambdaShape {
    pub(super) param_types: Vec<Option<Vec<Ty>>>,
    pub(super) expected_types: Vec<Option<Ty>>,
    pub(super) signatures: Vec<Option<&'static crate::types::FnSig>>,
    pub(super) receivers: Vec<Option<Ty>>,
    /// Per argument, whether the selected parameter inlines a lambda into the caller's frame.
    pub(super) inlined: Vec<bool>,
}

pub(super) fn module_member_lambda_shape(
    source: &dyn SymbolSource,
    member: &crate::libraries::LibraryMember,
    generic_member: Option<&GenericMemberPlan>,
    contextual: &ContextualCallShape,
    args: &[crate::ast::ExprId],
    names: Option<&[Option<String>]>,
    trailing_lambda: bool,
) -> Option<MemberLambdaShape> {
    let visible_indices = call_argument_parameter_indices(
        args.len(),
        contextual.params.len(),
        names,
        trailing_lambda,
        &contextual.call_sig,
    )?;
    let indices = visible_indices
        .into_iter()
        .map(|parameter| contextual.parameter_indices.get(parameter).copied())
        .collect::<Option<Vec<_>>>()?;
    Some(MemberLambdaShape {
        param_types: indices
            .iter()
            .map(|&parameter| {
                generic_member
                    .and_then(|plan| generic_member_lambda_params(source, plan, parameter))
                    .or_else(|| module_member_lambda_params(source, member, parameter))
            })
            .collect(),
        expected_types: indices
            .iter()
            .map(|&parameter| {
                generic_member
                    .and_then(|plan| {
                        plan.method
                            .signature
                            .params
                            .get(parameter)
                            .copied()
                            .map(|shape| {
                                crate::symbol_resolver::ty_subst_keep_unbound(
                                    shape,
                                    &plan.call_bindings,
                                )
                            })
                    })
                    .or_else(|| member.params.get(parameter).copied())
            })
            .collect(),
        signatures: indices
            .iter()
            .map(|&parameter| {
                let semantic = generic_member
                    .and_then(|plan| {
                        plan.method
                            .signature
                            .params
                            .get(parameter)
                            .copied()
                            .map(|shape| {
                                crate::symbol_resolver::ty_subst_keep_unbound(
                                    shape,
                                    &plan.call_bindings,
                                )
                            })
                    })
                    .or_else(|| {
                        member
                            .generic_sig
                            .as_ref()
                            .and_then(|signature| signature.params.get(parameter).copied())
                    })
                    .or_else(|| member.params.get(parameter).copied());
                match semantic {
                    Some(Ty::Fun(signature)) => Some(signature),
                    _ => None,
                }
            })
            .collect(),
        receivers: indices
            .iter()
            .map(|&parameter| {
                member
                    .call_sig
                    .lambda_receivers
                    .get(parameter)
                    .copied()
                    .flatten()
            })
            .collect(),
        inlined: indices
            .iter()
            .map(|&parameter| {
                member.inline.can_inline()
                    && crate::types::InlineParameterModifier::runs_parameter_in_caller_frame(
                        &member.call_sig.inline_modifiers,
                        parameter,
                    )
            })
            .collect(),
    })
}

/// Whether the parameter selected for source argument `argument` inlines a lambda into the
/// caller's frame, from the call's member shape, else its extension shape.
pub(super) fn shaped_argument_inlining(
    module: Option<&MemberLambdaShape>,
    extension: Option<&crate::symbol_resolver::LambdaCallShape>,
    argument: usize,
) -> Option<bool> {
    match module {
        Some(shape) => shape.inlined.get(argument).copied(),
        None => extension.and_then(|shape| shape.inlines_argument(argument)),
    }
}

/// The `crossinline`/`noinline` modifier a source parameter wrote.
pub(in crate::resolve) fn written_inline_modifier(
    parameter: &crate::ast::Param,
) -> crate::types::InlineParameterModifier {
    crate::types::InlineParameterModifier::written(
        parameter.is_materialized_lambda,
        parameter.is_crossinline,
    )
}

/// The functional shape at one argument position of a selected provider candidate. Lambdas consume
/// the split context/receiver/value inputs; callable references consume `callable_type`. Keeping one
/// carrier prevents the two syntax forms from running separate candidate selection paths.
#[derive(Clone, Debug)]
pub(super) struct FunctionalArgumentExpectation {
    pub(super) context_types: Vec<Ty>,
    pub(super) value_params: Vec<Ty>,
    pub(super) receiver: Option<Ty>,
    /// The parameter's declared result. A lambda whose expected result is `Unit` ends in statement
    /// position, which is what makes a trailing `when` with no `else` legal in every builder block.
    /// `None` when the parameter shape was recovered from metadata alone and carries no result.
    pub(super) result: Option<Ty>,
    pub(super) callable_type: Option<Ty>,
    pub(super) sam_conversion: bool,
    /// The selected parameter inlines a lambda into the caller's frame: the callable is inline and
    /// the parameter is neither `crossinline` nor `noinline`.
    pub(super) inlined: bool,
}

/// Derive the expected shape of a lambda argument from its selected candidate parameter. Kotlin
/// function types contribute context, receiver, value, and result types; Java SAMs contribute their
/// single abstract method. Any other parameter gives no expectation.
pub(super) fn functional_argument_expectation(
    platform: &dyn SemanticPlatform,
    call_sig: &CallSig,
    inline: bool,
    index: usize,
    param: Ty,
) -> Option<FunctionalArgumentExpectation> {
    let inlined = inline
        && crate::types::InlineParameterModifier::runs_parameter_in_caller_frame(
            &call_sig.inline_modifiers,
            index,
        );
    let has_receiver = call_sig
        .lambda_receiver_params
        .get(index)
        .copied()
        .unwrap_or(false);
    let metadata_receiver = call_sig.lambda_receivers.get(index).copied().flatten();
    match param.non_null() {
        Ty::Fun(signature) => {
            let (receiver, skip) = if has_receiver {
                // Prefer the call-site-substituted function input: it retains declaration type
                // arguments and nullability. The compact metadata receiver is recovery data for an
                // erased function shape, never a precision upgrade.
                let receiver = signature
                    .params
                    .get(signature.context_count)
                    .copied()
                    .or(metadata_receiver)?;
                (Some(receiver), signature.context_count + 1)
            } else {
                (None, signature.context_count)
            };
            Some(FunctionalArgumentExpectation {
                context_types: signature.params
                    [..signature.context_count.min(signature.params.len())]
                    .to_vec(),
                value_params: signature.params.get(skip..).unwrap_or_default().to_vec(),
                receiver,
                result: Some(signature.ret),
                callable_type: Some(Ty::Fun(signature)),
                sam_conversion: false,
                inlined,
            })
        }
        param if has_receiver => {
            // This recovery arm exists precisely because the function type was erased. Metadata
            // retains receiver/input facts but cannot supply a declared result.
            let receiver = metadata_receiver?;
            let context_count = call_sig
                .lambda_context_counts
                .get(index)
                .copied()
                .unwrap_or_default();
            let decoded = call_sig.lambda_param_types.get(index);
            let value_params = decoded
                .filter(|params| !params.is_empty())
                .map(|params| params.get(context_count + 1..).unwrap_or_default().to_vec())
                .unwrap_or_else(|| {
                    let arity = platform.function_like_arity(param).unwrap_or(1);
                    vec![Ty::Error; arity.saturating_sub(context_count + 1)]
                });
            Some(FunctionalArgumentExpectation {
                context_types: decoded
                    .map(|params| {
                        params
                            .get(..context_count.min(params.len()))
                            .unwrap_or_default()
                            .to_vec()
                    })
                    .unwrap_or_default(),
                value_params,
                receiver: Some(receiver),
                result: None,
                callable_type: None,
                sam_conversion: false,
                inlined,
            })
        }
        param => crate::symbol_resolver::semantic_sam_signature(platform, param).map(|sam| {
            let callable_type = Ty::fun_with_shape(
                sam.params.clone(),
                sam.ret,
                sam.context_count,
                sam.has_receiver,
                sam.suspend,
            );
            FunctionalArgumentExpectation {
                context_types: Vec::new(),
                value_params: sam.params,
                receiver: None,
                result: Some(sam.ret),
                callable_type: Some(callable_type),
                sam_conversion: true,
                inlined,
            }
        }),
    }
}
