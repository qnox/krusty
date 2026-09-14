//! Publication of provider inline-body contracts into stable checked FIR.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MappingFailure {
    UnsupportedPlan,
    IterationNeedsCallSiteProtocol,
    CollectionTransformNeedsCallSiteProtocol,
}

fn map_value(
    value: crate::libraries::InlineBodyValue,
    receiver_parameter: Option<usize>,
) -> Result<crate::fir::FirInlineValue, MappingFailure> {
    match value {
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
    }
}

fn map_parameter(
    parameter: usize,
    receiver_parameter: Option<usize>,
) -> Result<u32, MappingFailure> {
    match map_value(
        crate::libraries::InlineBodyValue::Parameter(parameter),
        receiver_parameter,
    )? {
        crate::fir::FirInlineValue::Parameter(parameter) => Ok(parameter),
        crate::fir::FirInlineValue::Receiver | crate::fir::FirInlineValue::Cause => {
            Err(MappingFailure::UnsupportedPlan)
        }
    }
}

fn map_call(
    call: &crate::libraries::InlineBodyCall,
    receiver_parameter: Option<usize>,
) -> Result<crate::fir::FirInlineCall, MappingFailure> {
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
                    map_value(value, receiver_parameter)
                        .map(crate::fir::FirInlineCallReceiver::Dispatch)
                }
                crate::libraries::InlineBodyCallReceiver::Extension(value) => {
                    map_value(value, receiver_parameter)
                        .map(crate::fir::FirInlineCallReceiver::Extension)
                }
            })
            .transpose()?,
        arguments: call
            .arguments
            .iter()
            .copied()
            .map(|value| map_value(value, receiver_parameter))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice(),
    })
}

fn map_recovery(
    recovery: &crate::libraries::InlineBodyRecovery,
    receiver_parameter: Option<usize>,
) -> Result<Box<crate::fir::FirInlineRecovery>, MappingFailure> {
    let caught = crate::fir::ResolvedTy::new(recovery.caught)
        .map_err(|_| MappingFailure::UnsupportedPlan)?;
    if caught.get().is_nullable() || caught.get().obj_internal().is_none() {
        return Err(MappingFailure::UnsupportedPlan);
    }
    let constructor = &recovery.constructor;
    let classifier = constructor.owner.ok_or(MappingFailure::UnsupportedPlan)?;
    let [constructor_parameter] = constructor.params.as_slice() else {
        return Err(MappingFailure::UnsupportedPlan);
    };
    let failure = map_call(&recovery.failure, receiver_parameter)?;
    if failure.receiver.is_some()
        || failure.suspend
        || failure.arguments.as_ref() != [crate::fir::FirInlineValue::Cause]
        || failure.parameters.as_ref() != [caught]
        || (failure.result.get() != *constructor_parameter
            && failure.result.get() != constructor_parameter.non_null())
    {
        return Err(MappingFailure::UnsupportedPlan);
    }
    Ok(Box::new(crate::fir::FirInlineRecovery {
        caught,
        constructor: constructor
            .external_identity
            .ok_or(MappingFailure::UnsupportedPlan)?,
        classifier,
        constructor_parameters: Box::new([crate::fir::ResolvedTy::new(*constructor_parameter)
            .map_err(|_| MappingFailure::UnsupportedPlan)?]),
        failure: Box::new(failure),
    }))
}

pub(super) fn publish(
    plan: Option<&crate::libraries::InlineBodyPlan>,
    receiver_parameter: Option<usize>,
) -> Result<Option<Box<crate::fir::FirInlineBodyPlan>>, MappingFailure> {
    let Some(plan) = plan else {
        return Ok(None);
    };
    Ok(Some(Box::new(match plan {
        crate::libraries::InlineBodyPlan::InvokeLambda {
            lambda_parameter,
            arguments,
            prologue,
            cleanup,
            cause,
            recovery,
            defaults,
            result,
        } => {
            if recovery.is_some()
                && (cause.is_some()
                    || !prologue.is_empty()
                    || !cleanup.is_empty()
                    || !defaults.is_empty()
                    || result.is_some())
            {
                return Err(MappingFailure::UnsupportedPlan);
            }
            crate::fir::FirInlineBodyPlan::InvokeLambda {
                lambda_parameter: map_parameter(*lambda_parameter, receiver_parameter)?,
                arguments: arguments
                    .iter()
                    .copied()
                    .map(|value| map_value(value, receiver_parameter))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                prologue: prologue
                    .iter()
                    .map(|call| map_call(call, receiver_parameter))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                cleanup: cleanup
                    .iter()
                    .map(|call| map_call(call, receiver_parameter))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                cause: cause
                    .map(crate::fir::ResolvedTy::new)
                    .transpose()
                    .map_err(|_| MappingFailure::UnsupportedPlan)?,
                recovery: recovery
                    .as_deref()
                    .map(|recovery| map_recovery(recovery, receiver_parameter))
                    .transpose()?,
                defaults: defaults
                    .iter()
                    .map(|default| {
                        Ok(crate::fir::FirInlineDefault {
                            parameter: map_parameter(default.parameter, receiver_parameter)?,
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
                result: result
                    .map(|value| map_value(value, receiver_parameter))
                    .transpose()?,
            }
        }
        crate::libraries::InlineBodyPlan::Iteration { .. } => {
            return Err(MappingFailure::IterationNeedsCallSiteProtocol)
        }
        // This plan also needs the call-site-selected iterator protocol and applied element type.
        // `selected_extension_call` publishes the complete checked variant in `calls`.
        crate::libraries::InlineBodyPlan::CollectionTransform { .. } => {
            return Err(MappingFailure::CollectionTransformNeedsCallSiteProtocol)
        }
    })))
}

pub(super) struct PublishedIteration {
    pub(super) lambda_parameter: u32,
    pub(super) index: Option<crate::fir::FirInlineIterationIndex>,
    pub(super) traversal: crate::fir::FirInlineIterationTraversal,
}

pub(super) fn publish_iteration(
    plan: &crate::libraries::InlineBodyPlan,
    receiver_parameter: Option<usize>,
    receiver: crate::types::Ty,
    element: crate::types::Ty,
) -> Result<PublishedIteration, MappingFailure> {
    let crate::libraries::InlineBodyPlan::Iteration {
        lambda_parameter,
        index,
        traversal,
    } = plan
    else {
        return Err(MappingFailure::UnsupportedPlan);
    };
    let index = index
        .as_ref()
        .map(|index| match index {
            crate::libraries::InlineIterationIndex::Unchecked => {
                Ok(crate::fir::FirInlineIterationIndex::Unchecked)
            }
            crate::libraries::InlineIterationIndex::Checked { overflow } => {
                if overflow.receiver.is_some()
                    || !overflow.arguments.is_empty()
                    || !overflow.callable.params.is_empty()
                    || overflow.callable.context_count != 0
                    || overflow.callable.ret != crate::types::Ty::Unit
                    || overflow.callable.suspend
                {
                    return Err(MappingFailure::UnsupportedPlan);
                }
                Ok(crate::fir::FirInlineIterationIndex::Checked {
                    overflow: Box::new(map_call(overflow, receiver_parameter)?),
                })
            }
        })
        .transpose()?;
    let publish_member = |member: &crate::libraries::LibraryMember,
                          receiver: crate::types::Ty|
     -> Result<crate::fir::FirInlineIterationMemberCall, MappingFailure> {
        Ok(crate::fir::FirInlineIterationMemberCall {
            declaration: member
                .external_identity
                .ok_or(MappingFailure::UnsupportedPlan)?,
            receiver: crate::fir::ResolvedTy::new(receiver)
                .map_err(|_| MappingFailure::UnsupportedPlan)?,
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
        })
    };
    let traversal = match traversal {
        crate::libraries::InlineIterationTraversal::Iterator {
            prepare,
            has_next,
            next,
        } => {
            let mut current = receiver;
            let mut published = Vec::with_capacity(prepare.len());
            for member in prepare {
                let call = publish_member(member, current)?;
                if !call.parameters.is_empty() {
                    return Err(MappingFailure::UnsupportedPlan);
                }
                current = call.result.get();
                published.push(call);
            }
            let has_next = publish_member(has_next, current)?;
            let next = publish_member(next, current)?;
            if !has_next.parameters.is_empty()
                || has_next.result.get() != crate::types::Ty::Boolean
                || !next.parameters.is_empty()
                || next.result.get() != element
            {
                return Err(MappingFailure::UnsupportedPlan);
            }
            crate::fir::FirInlineIterationTraversal::Iterator {
                prepare: published.into_boxed_slice(),
                has_next: Box::new(has_next),
                next: Box::new(next),
            }
        }
        crate::libraries::InlineIterationTraversal::Array => {
            if receiver.array_read_elem() != Some(element) {
                return Err(MappingFailure::UnsupportedPlan);
            }
            crate::fir::FirInlineIterationTraversal::Array
        }
        crate::libraries::InlineIterationTraversal::Counted { size, get } => {
            let size = publish_member(size, receiver)?;
            let get = publish_member(get, receiver)?;
            if !size.parameters.is_empty()
                || size.result.get() != crate::types::Ty::Int
                || get.parameters.as_ref()
                    != [crate::fir::ResolvedTy::new(crate::types::Ty::Int).expect("resolved Int")]
                || get.result.get() != element
            {
                return Err(MappingFailure::UnsupportedPlan);
            }
            crate::fir::FirInlineIterationTraversal::Counted {
                size: Box::new(size),
                get: Box::new(get),
            }
        }
    };
    Ok(PublishedIteration {
        lambda_parameter: map_parameter(*lambda_parameter, receiver_parameter)?,
        index,
        traversal,
    })
}

pub(super) fn publish_extension_iteration(
    plan: &crate::libraries::InlineBodyPlan,
    context_count: usize,
    receiver: crate::fir::ResolvedTy,
    parameters: &[crate::fir::ResolvedTy],
) -> Result<crate::fir::FirInlineBodyPlan, MappingFailure> {
    let crate::libraries::InlineBodyPlan::Iteration {
        lambda_parameter,
        index,
        ..
    } = plan
    else {
        return Err(MappingFailure::UnsupportedPlan);
    };
    // The provider plan numbers `(contexts..., receiver, values...)`, while the checked external
    // call target has already separated its receiver and therefore numbers only
    // `(contexts..., values...)`. Derive the action slot through the same mapping published into
    // FIR, then inspect the target's normalized parameter contract. This keeps resolver carrier
    // fields out of the checked boundary and makes ordinary and safe-call selectors consume the
    // same non-null selected receiver; the nullable source evaluation lives only in `FirReceiver`.
    let lambda_parameter = map_parameter(*lambda_parameter, Some(context_count))?;
    let action = parameters
        .get(lambda_parameter as usize)
        .map(|parameter| parameter.get())
        .and_then(|parameter| match parameter {
            crate::types::Ty::Fun(signature) => Some(signature),
            _ => None,
        })
        .filter(|signature| signature.params.len() == 1 + usize::from(index.is_some()))
        .ok_or(MappingFailure::UnsupportedPlan)?;
    let element = action
        .params
        .last()
        .copied()
        .ok_or(MappingFailure::UnsupportedPlan)?;
    let published = publish_iteration(plan, Some(context_count), receiver.get(), element)?;
    if published.lambda_parameter != lambda_parameter {
        return Err(MappingFailure::UnsupportedPlan);
    }
    Ok(crate::fir::FirInlineBodyPlan::Iteration {
        lambda_parameter: published.lambda_parameter,
        element: crate::fir::ResolvedTy::new(element)
            .map_err(|_| MappingFailure::UnsupportedPlan)?,
        index: published.index,
        traversal: published.traversal,
    })
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
        | crate::fir::FirInlineBodyPlan::Iteration {
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{checked_function_body_with_platform, jvm_stdlib_semantics};
    use super::*;
    use crate::fir::{FirCallTarget, FirExprId, FirExprKind, FirInlineBodyPlan};
    use crate::types::Ty;

    #[test]
    fn safe_iteration_call_retains_one_guarded_receiver_and_exact_plan() {
        let (body, _) = checked_function_body_with_platform(
            "fun total(values: HashMap<String, Int>?, definite: HashMap<String, Int>): Int {\n\
             \x20   var total = 0\n\
             \x20   values?.forEach { entry -> total += entry.key.length + entry.value }\n\
             \x20   definite.forEach { entry -> total += entry.key.length + entry.value }\n\
             \x20   return total\n\
             }\n",
            "total",
            jvm_stdlib_semantics(),
        );
        let safe = (0..body.expression_count())
            .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
            .find_map(|expression| match expression.kind {
                FirExprKind::SafeCall { receiver, selector } => Some((receiver, selector)),
                _ => None,
            })
            .expect("safe forEach call");
        let FirExprKind::Call(call) = &body.expr(safe.1).expect("safe selector").kind else {
            panic!("safe selector must retain the selected extension call")
        };
        let selected_receiver = call
            .extension_receiver
            .expect("selected extension receiver");
        assert_eq!(safe.0.value, selected_receiver.value);
        assert!(safe.0.conversion.is_none());
        assert!(
            body.expr(safe.0.value)
                .expect("guarded nullable evaluation")
                .ty
                .get()
                .is_nullable(),
            "the safe guard owns the nullable source evaluation"
        );
        assert!(call.dispatch_receiver.is_none());
        assert!(matches!(
            call.arguments.as_ref(),
            [crate::fir::FirCallArgument::Expression { parameter: 0, .. }]
        ));
        let FirCallTarget::External {
            receiver: Some(receiver),
            parameters,
            inline_plan: Some(plan),
            extension_receiver_parameter: None,
            ..
        } = &call.target
        else {
            panic!("safe extension must publish one external receiver and inline plan")
        };
        assert!(
            !receiver.get().is_nullable(),
            "the selected extension contract is non-null in the guarded branch"
        );
        assert_eq!(parameters.len(), 1, "the receiver is not a value parameter");
        let FirInlineBodyPlan::Iteration {
            lambda_parameter,
            traversal:
                crate::fir::FirInlineIterationTraversal::Iterator {
                    prepare,
                    has_next,
                    next,
                },
            ..
        } = plan.as_ref()
        else {
            panic!("safe Map extension must retain the exact iterator plan")
        };
        assert_eq!(*lambda_parameter, 0);
        assert_eq!(prepare.len(), 2);
        assert_eq!(prepare[0].receiver, *receiver);
        assert_eq!(prepare[1].receiver, prepare[0].result);
        assert_eq!(has_next.receiver, prepare[1].result);
        assert_eq!(next.receiver, prepare[1].result);

        let ordinary = (0..body.expression_count())
            .filter_map(|raw| {
                let id = FirExprId::from_raw(raw as u32);
                let expression = body.expr(id)?;
                (id != safe.1).then_some(expression)
            })
            .find_map(|expression| match &expression.kind {
                FirExprKind::Call(call)
                    if matches!(
                        call.target,
                        FirCallTarget::External {
                            inline_plan: Some(_),
                            ..
                        }
                    ) =>
                {
                    Some(call)
                }
                _ => None,
            })
            .expect("ordinary forEach call");
        let FirCallTarget::External {
            receiver: Some(ordinary_receiver),
            parameters: ordinary_parameters,
            inline_plan: Some(ordinary_plan),
            extension_receiver_parameter: None,
            ..
        } = &ordinary.target
        else {
            panic!("ordinary extension must publish one external receiver and inline plan")
        };
        assert_eq!(ordinary_receiver, receiver);
        assert_eq!(ordinary_parameters, parameters);
        assert_eq!(ordinary_plan, plan);
    }

    #[test]
    fn suspend_inline_finally_plan_is_fully_checked_and_opaque() {
        let classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB\n// WITH_COROUTINES");
        let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
        ));
        let (body, _) = checked_function_body_with_platform(
            "import kotlinx.coroutines.sync.Mutex\n\
             import kotlinx.coroutines.sync.withLock\n\
             suspend fun read(mutex: Mutex): String = mutex.withLock { \"OK\" }\n",
            "read",
            platform,
        );
        let plan = (0..body.expression_count()).find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let FirCallTarget::External {
                inline_plan: Some(plan),
                ..
            } = &call.target
            else {
                return None;
            };
            matches!(plan.as_ref(), FirInlineBodyPlan::InvokeLambda { cleanup, .. }
                if !cleanup.is_empty())
            .then_some(plan.as_ref())
        });
        let Some(FirInlineBodyPlan::InvokeLambda {
            lambda_parameter,
            arguments,
            prologue,
            cleanup,
            cause,
            recovery: None,
            defaults,
            result,
        }) = plan
        else {
            panic!("withLock must publish its selected structural plan in checked FIR")
        };
        assert_eq!(*lambda_parameter, 1);
        assert!(arguments.is_empty());
        assert_eq!(
            defaults.as_ref(),
            [crate::fir::FirInlineDefault {
                parameter: 0,
                value: crate::fir::FirInlineDefaultValue::Null,
            }]
        );
        assert_eq!(*cause, None);
        assert_eq!(*result, None);
        let ([enter], [cleanup]) = (prologue.as_ref(), cleanup.as_ref()) else {
            panic!("withLock must publish one enter and one cleanup call")
        };
        assert_eq!(enter.parameters.len(), 1);
        assert_eq!(cleanup.parameters.len(), 1);
        assert_eq!(enter.result.get(), Ty::Unit);
        assert_eq!(cleanup.result.get(), Ty::Unit);
        assert_eq!(
            enter.receiver,
            Some(crate::fir::FirInlineCallReceiver::Dispatch(
                crate::fir::FirInlineValue::Receiver,
            ))
        );
        assert_eq!(
            enter.arguments.as_ref(),
            [crate::fir::FirInlineValue::Parameter(0)]
        );
        assert_eq!(cleanup.receiver, enter.receiver);
        assert_eq!(cleanup.arguments, enter.arguments);
        assert!(enter.suspend);
        assert!(!cleanup.suspend);
        assert_ne!(enter.declaration, cleanup.declaration);
    }

    #[test]
    fn use_inline_finally_plan_keeps_its_semantic_extension_cleanup() {
        let (body, _) = checked_function_body_with_platform(
            "// WITH_STDLIB\n\
             import java.io.Closeable\n\
             fun read(resource: Closeable): String = resource.use { \"OK\" }\n",
            "read",
            jvm_stdlib_semantics(),
        );
        let plan = (0..body.expression_count()).find_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let FirCallTarget::External {
                inline_plan: Some(plan),
                ..
            } = &call.target
            else {
                return None;
            };
            matches!(
                plan.as_ref(),
                FirInlineBodyPlan::InvokeLambda { cause: Some(_), .. }
            )
            .then_some(plan.as_ref())
        });
        let Some(FirInlineBodyPlan::InvokeLambda {
            lambda_parameter,
            arguments,
            prologue,
            cleanup,
            cause,
            recovery: None,
            defaults,
            result,
        }) = plan
        else {
            panic!("Closeable.use must publish its complete plan in checked FIR")
        };
        assert_eq!(*lambda_parameter, 0);
        assert_eq!(arguments.as_ref(), [crate::fir::FirInlineValue::Receiver]);
        assert!(prologue.is_empty());
        assert_eq!(
            cause.map(crate::fir::ResolvedTy::get),
            Some(Ty::nullable(Ty::obj("kotlin/Throwable")))
        );
        assert!(defaults.is_empty());
        assert_eq!(*result, None);
        let [cleanup] = cleanup.as_ref() else {
            panic!("Closeable.use must publish exactly one cleanup call")
        };
        assert_eq!(
            cleanup.receiver,
            Some(crate::fir::FirInlineCallReceiver::Extension(
                crate::fir::FirInlineValue::Receiver,
            ))
        );
        assert_eq!(
            cleanup.arguments.as_ref(),
            [crate::fir::FirInlineValue::Cause]
        );
        assert_eq!(cleanup.parameters.len(), 1);
        assert_eq!(cleanup.result.get(), Ty::Unit);
        assert!(!cleanup.suspend);
    }

    #[test]
    fn present_inline_plan_conversion_failure_is_not_absence() {
        let member = crate::libraries::LibraryMember::new(
            "enter".to_string(),
            Vec::new(),
            Ty::Unit,
            "()V".to_string(),
        );
        let callable = crate::libraries::FunctionInfo::classifier_member(
            crate::libraries::FnKind::Member,
            crate::types::type_name("test/Owner"),
            member,
        )
        .callable;
        let plan = crate::libraries::InlineBodyPlan::InvokeLambda {
            lambda_parameter: 0,
            arguments: Vec::new(),
            prologue: vec![crate::libraries::InlineBodyCall {
                callable: Box::new(callable),
                receiver: None,
                arguments: Vec::new(),
            }],
            cleanup: Vec::new(),
            cause: None,
            recovery: None,
            defaults: Vec::new(),
            result: None,
        };

        assert_eq!(publish(None, None), Ok(None));
        assert_eq!(
            publish(Some(&plan), None),
            Err(MappingFailure::UnsupportedPlan),
            "a present plan without stable member identities is a publication error",
        );
    }

    #[test]
    fn caught_type_does_not_come_from_the_cleanup_parameter() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let mut cleanup = crate::libraries::LibraryCallable::library(
            crate::types::type_name("test/CleanupKt"),
            "cleanup",
            vec![Ty::nullable(Ty::obj("java/io/Closeable")), any],
            Ty::Unit,
            Ty::Unit,
            "(Ljava/io/Closeable;Ljava/lang/Object;)V",
        );
        cleanup.external_identity = Some(crate::fir::ExternalCallableId::from_raw(7));
        let plan = crate::libraries::InlineBodyPlan::InvokeLambda {
            lambda_parameter: 1,
            arguments: vec![crate::libraries::InlineBodyValue::Parameter(0)],
            prologue: Vec::new(),
            cleanup: vec![crate::libraries::InlineBodyCall {
                callable: Box::new(cleanup),
                receiver: Some(crate::libraries::InlineBodyCallReceiver::Extension(
                    crate::libraries::InlineBodyValue::Parameter(0),
                )),
                arguments: vec![crate::libraries::InlineBodyValue::Cause],
            }],
            cause: Some(Ty::nullable(Ty::obj("kotlin/Throwable"))),
            recovery: None,
            defaults: Vec::new(),
            result: None,
        };
        let Some(plan) = publish(Some(&plan), Some(0)).expect("valid checked plan") else {
            panic!("present provider plan must remain present")
        };
        let FirInlineBodyPlan::InvokeLambda { cause, cleanup, .. } = plan.as_ref() else {
            panic!("expected generalized invocation plan")
        };
        assert_eq!(
            cause.map(crate::fir::ResolvedTy::get),
            Some(Ty::nullable(Ty::obj("kotlin/Throwable")))
        );
        let [cleanup] = cleanup.as_ref() else {
            panic!("one cleanup call")
        };
        assert_eq!(cleanup.parameters.len(), 1);
        assert_eq!(cleanup.parameters[0].get(), any);
    }
}
