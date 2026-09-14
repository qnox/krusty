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
            cause,
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
            cause: cause
                .map(crate::fir::ResolvedTy::new)
                .transpose()
                .map_err(|_| MappingFailure::UnsupportedPlan)?,
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{checked_function_body_with_platform, jvm_stdlib_semantics};
    use super::*;
    use crate::fir::{FirCallTarget, FirExprId, FirExprKind, FirInlineBodyPlan};
    use crate::types::Ty;

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
