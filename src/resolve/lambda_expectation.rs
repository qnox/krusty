use crate::libraries::{CallSig, SemanticPlatform};
use crate::types::Ty;

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
}

/// Derive the expected shape of a lambda argument from its selected candidate parameter. Kotlin
/// function types contribute context, receiver, value, and result types; Java SAMs contribute their
/// single abstract method. Any other parameter gives no expectation.
pub(super) fn functional_argument_expectation(
    platform: &dyn SemanticPlatform,
    call_sig: &CallSig,
    index: usize,
    param: Ty,
) -> Option<FunctionalArgumentExpectation> {
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
            }
        }),
    }
}
