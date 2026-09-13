//! Publication of provider inline-body contracts into stable checked FIR.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MappingFailure {
    UnsupportedPlan,
    CollectionTransformNeedsCallSiteProtocol,
}

pub(super) fn publish(
    plan: Option<&crate::libraries::InlineBodyPlan>,
    receiver_parameter: Option<usize>,
) -> Result<Option<Box<crate::fir::FirInlineBodyPlan>>, MappingFailure> {
    let map_value = |parameter: usize| {
        if receiver_parameter == Some(parameter) {
            Ok(crate::fir::FirInlineValue::Receiver)
        } else {
            let parameter = parameter
                .checked_sub(usize::from(
                    receiver_parameter.is_some_and(|receiver| parameter > receiver),
                ))
                .ok_or(MappingFailure::UnsupportedPlan)?;
            Ok(crate::fir::FirInlineValue::Parameter(
                u32::try_from(parameter).map_err(|_| MappingFailure::UnsupportedPlan)?,
            ))
        }
    };
    let map_parameter = |parameter| match map_value(parameter)? {
        crate::fir::FirInlineValue::Parameter(parameter) => Ok(parameter),
        crate::fir::FirInlineValue::Receiver => Err(MappingFailure::UnsupportedPlan),
    };
    let member_call = |member: &crate::libraries::LibraryMember| {
        Ok(crate::fir::FirInlineMemberCall {
            declaration: member
                .external_identity
                .ok_or(MappingFailure::UnsupportedPlan)?,
            parameters: member
                .params
                .iter()
                .copied()
                .map(crate::fir::ResolvedTy::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| MappingFailure::UnsupportedPlan)?
                .into_boxed_slice(),
            result: crate::fir::ResolvedTy::new(member.ret)
                .map_err(|_| MappingFailure::UnsupportedPlan)?,
            suspend: member.suspend(),
        })
    };
    let Some(plan) = plan else {
        return Ok(None);
    };
    Ok(Some(Box::new(match plan {
        crate::libraries::InlineBodyPlan::InvokeLambda {
            lambda_parameter,
            argument_parameters,
            return_parameter,
        } => crate::fir::FirInlineBodyPlan::InvokeLambda {
            lambda_parameter: map_parameter(*lambda_parameter)?,
            arguments: argument_parameters
                .iter()
                .copied()
                .map(map_value)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
            result: return_parameter.map(map_value).transpose()?,
        },
        crate::libraries::InlineBodyPlan::SuspendBeforeLambdaFinally {
            lambda_parameter,
            state,
            enter,
            cleanup,
        } => crate::fir::FirInlineBodyPlan::SuspendBeforeLambdaFinally {
            lambda_parameter: map_parameter(*lambda_parameter)?,
            state: match state {
                None => None,
                Some(state) => Some(crate::fir::FirInlineBodyState {
                    parameter: map_parameter(state.parameter)?,
                    default: match state.default {
                        crate::libraries::DefaultValue::Null => {
                            crate::fir::FirInlineDefaultValue::Null
                        }
                        _ => return Err(MappingFailure::UnsupportedPlan),
                    },
                }),
            },
            enter: member_call(enter)?,
            cleanup: member_call(cleanup)?,
        },
        // This plan also needs the call-site-selected iterator protocol and applied element type.
        // `selected_extension_call` publishes the complete checked variant in `calls`.
        crate::libraries::InlineBodyPlan::CollectionTransform { .. } => {
            return Err(MappingFailure::CollectionTransformNeedsCallSiteProtocol)
        }
    })))
}
