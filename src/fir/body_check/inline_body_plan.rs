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
    let map_value = |value: crate::libraries::InlineBodyValue| match value {
        crate::libraries::InlineBodyValue::Cause => Ok(crate::fir::FirInlineValue::Cause),
        crate::libraries::InlineBodyValue::Parameter(parameter) => {
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
        }
    };
    let map_parameter =
        |parameter| match map_value(crate::libraries::InlineBodyValue::Parameter(parameter))? {
            crate::fir::FirInlineValue::Parameter(parameter) => Ok(parameter),
            crate::fir::FirInlineValue::Receiver | crate::fir::FirInlineValue::Cause => {
                Err(MappingFailure::UnsupportedPlan)
            }
        };
    let call = |call: &crate::libraries::InlineBodyCall| {
        let mut parameters = call.callable.params.clone();
        if matches!(
            call.receiver,
            Some(crate::libraries::InlineBodyCallReceiver::Extension(_))
        ) {
            if call.callable.context_count >= parameters.len() {
                return Err(MappingFailure::UnsupportedPlan);
            }
            parameters.remove(call.callable.context_count);
        }
        Ok(crate::fir::FirInlineCall {
            declaration: call
                .callable
                .external_identity
                .ok_or(MappingFailure::UnsupportedPlan)?,
            parameters: parameters
                .into_iter()
                .map(crate::fir::ResolvedTy::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| MappingFailure::UnsupportedPlan)?
                .into_boxed_slice(),
            result: crate::fir::ResolvedTy::new(call.callable.ret)
                .map_err(|_| MappingFailure::UnsupportedPlan)?,
            suspend: call.callable.suspend,
            receiver: call
                .receiver
                .map(|receiver| match receiver {
                    crate::libraries::InlineBodyCallReceiver::Dispatch(value) => {
                        map_value(value).map(crate::fir::FirInlineCallReceiver::Dispatch)
                    }
                    crate::libraries::InlineBodyCallReceiver::Extension(value) => {
                        map_value(value).map(crate::fir::FirInlineCallReceiver::Extension)
                    }
                })
                .transpose()?,
            arguments: call
                .arguments
                .iter()
                .copied()
                .map(map_value)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
        })
    };
    let Some(plan) = plan else {
        return Ok(None);
    };
    Ok(Some(Box::new(match plan {
        crate::libraries::InlineBodyPlan::InvokeLambda {
            lambda_parameter,
            arguments,
            prologue,
            cleanup,
            records_cause,
            defaults,
            result,
        } => crate::fir::FirInlineBodyPlan::InvokeLambda {
            lambda_parameter: map_parameter(*lambda_parameter)?,
            arguments: arguments
                .iter()
                .copied()
                .map(map_value)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
            prologue: prologue
                .iter()
                .map(call)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
            cleanup: cleanup
                .iter()
                .map(call)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
            records_cause: *records_cause,
            defaults: defaults
                .iter()
                .map(|default| {
                    Ok(crate::fir::FirInlineDefault {
                        parameter: map_parameter(default.parameter)?,
                        value: match default.value {
                            crate::libraries::DefaultValue::Null => {
                                crate::fir::FirInlineDefaultValue::Null
                            }
                            _ => return Err(MappingFailure::UnsupportedPlan),
                        },
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
            result: result.map(map_value).transpose()?,
        },
        // This plan also needs the call-site-selected iterator protocol and applied element type.
        // `selected_extension_call` publishes the complete checked variant in `calls`.
        crate::libraries::InlineBodyPlan::CollectionTransform { .. } => {
            return Err(MappingFailure::CollectionTransformNeedsCallSiteProtocol)
        }
    })))
}

/// Commit applicability while both the complete checked call and a literal's checked body are
/// available. `None` after this operation is an ordinary selected dependency call, not a lowering
/// fallback. A present plan is therefore an unconditional obligation for common lowering.
pub(super) fn finalize(
    body: &crate::fir::FirBody,
    call: &mut crate::fir::FirCall,
) -> Result<(), MappingFailure> {
    let crate::fir::FirCallTarget::External { inline_plan, .. } = &mut call.target else {
        return Ok(());
    };
    let Some(plan) = inline_plan.as_deref() else {
        return Ok(());
    };
    let lambda_parameter = match plan {
        crate::fir::FirInlineBodyPlan::InvokeLambda {
            lambda_parameter, ..
        }
        | crate::fir::FirInlineBodyPlan::ForEach {
            lambda_parameter, ..
        }
        | crate::fir::FirInlineBodyPlan::CollectionTransform {
            lambda_parameter, ..
        } => *lambda_parameter,
    };
    let collection_transform = matches!(
        plan,
        crate::fir::FirInlineBodyPlan::CollectionTransform { .. }
    );
    let mut selected_value = None;
    for argument in &call.arguments {
        match argument {
            crate::fir::FirCallArgument::Expression {
                parameter, value, ..
            } if *parameter == lambda_parameter && selected_value.is_none() => {
                selected_value = Some(*value);
            }
            crate::fir::FirCallArgument::Expression { parameter, .. }
            | crate::fir::FirCallArgument::Default { parameter, .. }
            | crate::fir::FirCallArgument::Vararg { parameter, .. }
                if *parameter == lambda_parameter =>
            {
                return Err(MappingFailure::UnsupportedPlan);
            }
            _ => {}
        }
    }
    let value = selected_value.ok_or(MappingFailure::UnsupportedPlan)?;
    let literal_body = body.expr(value).and_then(|argument| match &argument.kind {
        crate::fir::FirExprKind::Lambda { body, .. } => Some(body.as_ref()),
        _ => None,
    });
    if !literal_body.is_some_and(|body| !collection_transform || body.direct_suspension) {
        *inline_plan = None;
    }
    Ok(())
}
