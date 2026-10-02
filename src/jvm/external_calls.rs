use super::classpath::{Classpath, ExternalCallableKind};
use crate::fir::ExternalCallableId;
use crate::ir::{Callee, IrCheckedOperation, IrExpr, IrFile};
use crate::types::InlineParameterModifier;

use super::default_call_operands::{DefaultCallOperand, DefaultCallOperands};

mod arguments;
pub(super) use arguments::ExternalRealizationError;
use arguments::{
    copy_call_facts, materialize_constructor_defaults, materialize_omitted_arguments,
    ExternalDependencyTarget,
};

pub(super) fn realize(
    ir: &mut IrFile,
    classifiers: &dyn crate::backend::BackendClassifierSource,
    classpath: &Classpath,
    callables: &crate::backend::CheckedBackendCallables,
    default_call_operands: &mut DefaultCallOperands,
) -> Result<(), ExternalRealizationError> {
    // Body-local, anonymous, and backend-generated receivers are complete only in this file's
    // common IR. Freeze them before mutating call nodes and combine them with the stable
    // module/dependency snapshot for every dispatch-owner decision below.
    let dispatch_classifiers =
        crate::jvm::member_dispatch::CheckedDispatchClassifiers::new(ir, classifiers);
    let expression_count = ir.exprs.len();
    for index in 0..expression_count {
        let expression = u32::try_from(index).expect("too many common IR expressions");
        let (defaults, default_prefix_count) = match &ir.exprs[index] {
            IrExpr::New {
                defaults,
                default_prefix_count,
                ..
            } => (defaults.to_vec(), *default_prefix_count),
            _ => (Vec::new(), 0),
        };
        if let IrExpr::New {
            external_target: Some(target),
            ..
        } = ir.exprs[index]
        {
            let callable = callables.callable(target).cloned().ok_or_else(|| {
                crate::trace_compiler!(
                    "fir",
                    "missing external constructor realization id={target:?} expression={index}"
                );
                target
            })?;
            if callable.kind != ExternalCallableKind::Constructor {
                return Err(target.into());
            }
            let physical_name = callable.physical_name().to_string();
            if matches!(
                callable.member_realization,
                crate::libraries::MemberRealization::Direct {
                    pass_receiver: false
                }
            ) && physical_name != "<init>"
            {
                let (owner, name, descriptor, real_params, suffix) = if defaults.is_empty() {
                    (
                        callable.physical_owner,
                        physical_name,
                        if callable.descriptor.is_empty() {
                            crate::jvm::names::method_descriptor(
                                &callable.physical_params,
                                callable.physical_ret,
                            )
                        } else {
                            callable.descriptor
                        },
                        Vec::new(),
                        Vec::new(),
                    )
                } else {
                    let default = callable.default_realization.as_deref().ok_or(target)?;
                    if default.name == "<init>" || default.mask_count == 0 {
                        return Err(target.into());
                    }
                    let mut masks = vec![0i32; default.mask_count];
                    for parameter in &defaults {
                        let parameter = *parameter as usize;
                        let mask = masks.get_mut(parameter / 32).ok_or(target)?;
                        *mask |= 1i32 << (parameter % 32);
                    }
                    let mut suffix = masks
                        .into_iter()
                        .map(|mask| ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(mask))))
                        .collect::<Vec<_>>();
                    suffix.push(ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null)));
                    (
                        default.owner,
                        default.name.clone(),
                        default.descriptor.clone(),
                        default.real_params.clone(),
                        suffix,
                    )
                };
                let supplied = match &ir.exprs[index] {
                    IrExpr::New { args, .. } => args.clone(),
                    _ => unreachable!(),
                };
                let realized_args = if defaults.is_empty() {
                    supplied
                } else {
                    materialize_constructor_defaults(
                        ir,
                        &real_params,
                        supplied,
                        &defaults,
                        default_prefix_count,
                        target,
                    )?
                };
                let IrExpr::New {
                    args,
                    external_target,
                    ..
                } = &mut ir.exprs[index]
                else {
                    unreachable!()
                };
                *args = realized_args;
                args.extend(suffix);
                let args = std::mem::take(args);
                *external_target = None;
                ir.exprs[index] = IrExpr::Call {
                    callee: Callee::Static {
                        owner,
                        name,
                        descriptor,
                        inline: callable.inline,
                    },
                    dispatch_receiver: None,
                    args,
                };
                continue;
            }
            let default = if defaults.is_empty() {
                None
            } else {
                let default = callable.default_realization.as_deref().ok_or(target)?;
                if default.name != "<init>" || default.mask_count == 0 {
                    return Err(target.into());
                }
                Some(default)
            };
            let constructor = defaults
                .is_empty()
                .then_some(callable.nonvirtual_realization.as_deref())
                .flatten();
            if constructor.is_some_and(|constructor| constructor.owner != callable.physical_owner) {
                return Err(target.into());
            }
            let realization_operands = if let Some(default) = default {
                let mut masks = vec![0i32; default.mask_count];
                for parameter in &defaults {
                    let parameter = *parameter as usize;
                    let mask = masks.get_mut(parameter / 32).ok_or(target)?;
                    *mask |= 1i32 << (parameter % 32);
                }
                let mut operands = masks
                    .into_iter()
                    .map(|mask| ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(mask))))
                    .collect::<Vec<_>>();
                operands.push(ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null)));
                operands
            } else if constructor.is_some() {
                vec![ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null))]
            } else {
                Vec::new()
            };
            let supplied = match &ir.exprs[index] {
                IrExpr::New { args, .. } => args.clone(),
                _ => unreachable!(),
            };
            let realized_arguments = if let Some(default) = default {
                materialize_constructor_defaults(
                    ir,
                    &default.real_params,
                    supplied,
                    &defaults,
                    default_prefix_count,
                    target,
                )?
            } else {
                supplied
            };
            let IrExpr::New {
                args,
                ctor_desc,
                external_target,
                defaults,
                default_prefix_count,
                ..
            } = &mut ir.exprs[index]
            else {
                unreachable!()
            };
            *args = realized_arguments;
            args.extend(realization_operands);
            *ctor_desc = Some(if let Some(default) = default {
                default.descriptor.clone()
            } else if let Some(constructor) = constructor {
                constructor.descriptor.clone()
            } else if callable.descriptor.is_empty() {
                crate::jvm::names::method_descriptor(
                    &callable.physical_params,
                    crate::types::Ty::Unit,
                )
            } else {
                callable.descriptor
            });
            *external_target = None;
            *defaults = Box::new([]);
            *default_prefix_count = 0;
            continue;
        }
        let property = match ir.exprs[index].clone() {
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                dispatch,
                receiver,
                arguments,
                parameters,
                result,
                source_receiver,
            }) => Some((
                target,
                false,
                dispatch,
                receiver,
                arguments,
                parameters,
                result,
                source_receiver,
            )),
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyWrite {
                target,
                dispatch,
                receiver,
                arguments,
                parameters,
                result,
                source_receiver,
            }) => Some((
                target,
                true,
                dispatch,
                receiver,
                arguments,
                parameters,
                result,
                source_receiver,
            )),
            _ => None,
        };
        let mut property_dispatch = crate::ir::IrPropertyDispatch::Ordinary;
        if let Some((
            property,
            write,
            dispatch,
            receiver,
            arguments,
            parameters,
            result,
            source_receiver,
        )) = property
        {
            property_dispatch = dispatch;
            let realization = callables
                .property(property)
                .ok_or(ExternalDependencyTarget::Property(property))?;
            let target = if write {
                realization
                    .setter
                    .ok_or(ExternalDependencyTarget::Property(property))?
            } else {
                realization.getter
            };
            ir.exprs[index] = IrExpr::Call {
                callee: Callee::External {
                    target,
                    default_provider: None,
                    params: parameters,
                    ret: result,
                    substitutions: Vec::new(),
                    defaults: Vec::new(),
                    extension_receiver_parameter: None,
                },
                dispatch_receiver: receiver,
                args: arguments,
            };
            if let Some(source_receiver) = source_receiver {
                ir.ext_call_source_receiver
                    .insert(expression, source_receiver);
            }
        }
        let (
            target,
            default_provider,
            semantic_params,
            semantic_ret,
            substitutions,
            defaults,
            extension_receiver_parameter,
        ) = match &ir.exprs[index] {
            IrExpr::Call {
                callee:
                    Callee::External {
                        target,
                        default_provider,
                        params,
                        ret,
                        substitutions,
                        defaults,
                        extension_receiver_parameter,
                    },
                ..
            } => (
                *target,
                *default_provider,
                params.clone(),
                *ret,
                substitutions.clone(),
                defaults.clone(),
                *extension_receiver_parameter,
            ),
            _ => continue,
        };
        let callable = callables.callable(target).cloned().ok_or_else(|| {
            crate::trace_compiler!(
                "fir",
                "missing external call realization id={target:?} expression={index} semantic_result={semantic_ret:?}"
            );
            target
        })?;
        let physical_name = callable.physical_name().to_string();
        let kind = callable.kind;
        publish_reified_substitutions(ir, expression, target, &callable, &substitutions);
        let declared_params = callable.declared_params.clone();
        let inline_modifiers = callable.inline_modifiers.clone();
        let member_realization = callable.member_realization;
        let semantic_role = callable.semantic_role;
        let descriptor = if callable.descriptor.is_empty() {
            crate::jvm::names::method_descriptor(&callable.physical_params, callable.physical_ret)
        } else {
            callable.descriptor.clone()
        };
        // A `String.plus` that reaches this boundary is a callable-reference adapter body (source
        // calls lower as concatenations): it is the same two-part concatenation.
        if callable.compiler_intrinsic == Some(crate::libraries::CompilerIntrinsic::StringPlus)
            || member_realization
                == crate::libraries::MemberRealization::Intrinsic(
                    crate::libraries::CompilerIntrinsic::StringPlus,
                )
        {
            let IrExpr::Call {
                dispatch_receiver: Some(receiver),
                args,
                ..
            } = &ir.exprs[index]
            else {
                return Err(target.into());
            };
            let [argument] = args[..] else {
                return Err(target.into());
            };
            let parts = vec![*receiver, argument];
            ir.ext_call_source_receiver.remove(&expression);
            ir.exprs[index] = IrExpr::StringConcat(parts);
            continue;
        }
        // A builtin scalar member (`Int.times`, `Boolean.not()`) is a selected Kotlin declaration
        // with no JVM method. Checked FIR publishes the operation for an ordinary call; a callable-
        // reference adapter body keeps the provider identity in an ordinary external-call node, so
        // realize that exact declaration with the common primitive operation at this target
        // boundary. `String.get`, the array element accessors, and a scalar's `hashCode` keep
        // their own intrinsic calls below.
        if let crate::libraries::MemberRealization::Intrinsic(intrinsic) = member_realization {
            if !matches!(
                intrinsic,
                crate::libraries::CompilerIntrinsic::StringGet
                    | crate::libraries::CompilerIntrinsic::ArrayGet
                    | crate::libraries::CompilerIntrinsic::ArraySet
                    | crate::libraries::CompilerIntrinsic::ArraySize
                    | crate::libraries::CompilerIntrinsic::PrimitiveIteratorNext
                    | crate::libraries::CompilerIntrinsic::NullableAnyToString
                    | crate::libraries::CompilerIntrinsic::PrimitiveHashCode
            ) {
                let (receiver, arguments) = match &ir.exprs[index] {
                    IrExpr::Call {
                        dispatch_receiver: Some(receiver),
                        args,
                        ..
                    } if kind == ExternalCallableKind::Member && defaults.is_empty() => {
                        (*receiver, args.clone())
                    }
                    _ => return Err(target.into()),
                };
                let receiver_ty = ir
                    .ext_call_source_receiver
                    .get(&expression)
                    .copied()
                    .ok_or(target)?;
                let operation = super::builtin_member_operations::operation(
                    ir,
                    intrinsic,
                    super::builtin_member_operations::BuiltinMemberOperands {
                        receiver,
                        receiver_ty,
                        arguments: &arguments,
                        parameters: &semantic_params,
                        result: semantic_ret,
                    },
                )
                .ok_or(target)?;
                ir.ext_call_source_receiver.remove(&expression);
                ir.exprs[index] = operation;
                continue;
            }
        }
        if kind == ExternalCallableKind::StaticFieldRead {
            if !defaults.is_empty()
                || !matches!(
                    &ir.exprs[index],
                    IrExpr::Call {
                        dispatch_receiver: None,
                        args,
                        ..
                    } if args.is_empty()
                )
            {
                return Err(target.into());
            }
            ir.property_external_accessors.insert(expression, target);
            ir.exprs[index] = IrExpr::PropertyRead {
                receiver: None,
                owner: callable.physical_owner,
                name: physical_name.clone(),
                ty: semantic_ret,
                interface: false,
                operation: Some(expression),
            };
            continue;
        }
        if kind == ExternalCallableKind::StaticFieldWrite {
            let value = match &ir.exprs[index] {
                IrExpr::Call {
                    dispatch_receiver: None,
                    args,
                    ..
                } if defaults.is_empty() && args.len() == 1 => args[0],
                _ => return Err(target.into()),
            };
            let property_ty = callable.params.first().copied().ok_or(target)?;
            ir.property_external_accessors.insert(expression, target);
            ir.exprs[index] = IrExpr::PropertyWrite {
                receiver: None,
                owner: callable.physical_owner,
                name: physical_name.clone(),
                value,
                ty: property_ty,
                interface: false,
                operation: Some(expression),
            };
            continue;
        }
        if matches!(
            kind,
            ExternalCallableKind::InstanceFieldRead | ExternalCallableKind::InstanceFieldWrite
        ) {
            if !defaults.is_empty() {
                return Err(target.into());
            }
            let (receiver, arguments) = match &ir.exprs[index] {
                IrExpr::Call {
                    dispatch_receiver: Some(receiver),
                    args,
                    ..
                } => (*receiver, args.clone()),
                _ => return Err(target.into()),
            };
            let operation = Some(index as crate::ir::ExprId);
            ir.property_external_accessors.insert(expression, target);
            match kind {
                ExternalCallableKind::InstanceFieldRead if arguments.is_empty() => {
                    ir.exprs[index] = IrExpr::PropertyRead {
                        receiver: Some(receiver),
                        owner: callable.physical_owner,
                        name: physical_name.clone(),
                        ty: semantic_ret,
                        interface: false,
                        operation,
                    };
                }
                ExternalCallableKind::InstanceFieldWrite if arguments.len() == 1 => {
                    let property_ty = callable.params.first().copied().ok_or(target)?;
                    ir.exprs[index] = IrExpr::PropertyWrite {
                        receiver: Some(receiver),
                        owner: callable.physical_owner,
                        name: physical_name.clone(),
                        value: arguments[0],
                        ty: property_ty,
                        interface: false,
                        operation,
                    };
                }
                ExternalCallableKind::InstanceFieldRead
                | ExternalCallableKind::InstanceFieldWrite => return Err(target.into()),
                _ => unreachable!(),
            }
            continue;
        }
        if !defaults.is_empty() {
            crate::trace_compiler!(
                "default_semantics",
                "realize external default target={target:?} provider={default_provider:?} kind={kind:?} owner={} name={} descriptor={} omitted={defaults:?} bridge={:?}",
                callable.physical_owner,
                physical_name,
                callable.descriptor,
                callable.default_realization,
            );
            let default = if let Some(provider) = default_provider {
                callables
                    .callable(provider)
                    .and_then(|fact| fact.default_realization.as_deref())
                    .cloned()
                    .ok_or(provider)?
            } else {
                callable
                    .default_realization
                    .as_deref()
                    .cloned()
                    .ok_or(target)?
            };
            // Reconstruct the selected bridge's complete physical parameter prefix here. Common IR
            // carries only supplied operands; a value class such as `Duration` may require a `long`
            // zero even though its semantic type is a reference.
            let extension_receiver = extension_receiver_parameter
                .map(|parameter| parameter as usize)
                .or_else(|| {
                    (kind == ExternalCallableKind::Extension)
                        .then_some(callable.source_receiver)
                        .flatten()
                        .map(|_| callable.context_count)
                });
            let member_extension =
                kind == ExternalCallableKind::Member && extension_receiver_parameter.is_some();
            let omitted_parameters = defaults
                .iter()
                .map(|default_parameter| {
                    let source_parameter = *default_parameter as usize;
                    let physical_parameter = source_parameter
                        + usize::from(
                            extension_receiver.is_some_and(|receiver| source_parameter >= receiver),
                        );
                    Ok(if member_extension {
                        physical_parameter
                    } else {
                        source_parameter
                    } as u32)
                })
                .collect::<Result<Vec<_>, ExternalCallableId>>()?;
            let supplied = match &ir.exprs[index] {
                IrExpr::Call { args, .. } => args.clone(),
                _ => unreachable!(),
            };
            let mut default_parameters = default.real_params.clone();
            if kind == ExternalCallableKind::Extension {
                if callable.context_count >= default_parameters.len() {
                    return Err(target.into());
                }
                // The extension receiver is a source slot the call already supplied. Masks and the
                // marker stay on the realization and are appended after this prefix.
                default_parameters.remove(callable.context_count);
            }
            let source_plan = crate::libraries::physical_parameter_plan::source_parameter_plan(
                default_parameters.len(),
            );
            let default_parameters = super::physical_call_arguments::publish_dependency_parameters(
                ir,
                expression,
                default_parameters,
                Some(source_plan.as_ref()),
                supplied.len(),
                &omitted_parameters,
            )
            .map_err(|detail| ExternalRealizationError::Arguments {
                target: target.into(),
                detail,
            })?;
            let mut realized_arguments = materialize_omitted_arguments(
                ir,
                &default_parameters,
                supplied,
                &omitted_parameters,
                target,
            )?;
            let mut operand_plan = realized_arguments
                .iter()
                .copied()
                .enumerate()
                .map(|(parameter, argument)| {
                    if omitted_parameters.contains(&(parameter as u32)) {
                        DefaultCallOperand::synthesized(argument)
                    } else {
                        DefaultCallOperand::supplied(argument)
                    }
                })
                .collect::<Vec<_>>();
            {
                let IrExpr::Call {
                    dispatch_receiver,
                    args,
                    ..
                } = &mut ir.exprs[index]
                else {
                    unreachable!()
                };
                *args = std::mem::take(&mut realized_arguments);
                match kind {
                    ExternalCallableKind::TopLevel => {}
                    ExternalCallableKind::Extension => {
                        let receiver = dispatch_receiver.take().ok_or(target)?;
                        let position = callable.context_count.min(args.len());
                        args.insert(position, receiver);
                        operand_plan.insert(position, DefaultCallOperand::supplied(receiver));
                        ir.static_extension_receivers
                            .insert(expression, position as u32);
                    }
                    ExternalCallableKind::Member => {
                        let receiver = dispatch_receiver.take().ok_or(target)?;
                        args.insert(0, receiver);
                        operand_plan.insert(0, DefaultCallOperand::supplied(receiver));
                        if let Some(position) = extension_receiver {
                            ir.static_extension_receivers
                                .insert(expression, (position + 1) as u32);
                        }
                    }
                    ExternalCallableKind::Constructor
                    | ExternalCallableKind::InstanceFieldRead
                    | ExternalCallableKind::InstanceFieldWrite
                    | ExternalCallableKind::StaticFieldRead
                    | ExternalCallableKind::StaticFieldWrite => {
                        return Err(target.into());
                    }
                }
            }
            let mut masks = vec![0i32; default.mask_count];
            for parameter in defaults {
                let word = parameter as usize / 32;
                let bit = parameter % 32;
                let mask = masks.get_mut(word).ok_or(target)?;
                *mask |= 1i32 << bit;
            }
            let mask_values = masks
                .into_iter()
                .map(|mask| ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(mask))))
                .collect::<Vec<_>>();
            let marker = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
            operand_plan.extend(
                mask_values
                    .iter()
                    .copied()
                    .map(DefaultCallOperand::synthesized_abi),
            );
            operand_plan.push(DefaultCallOperand::synthesized_abi(marker));
            {
                let IrExpr::Call { callee, args, .. } = &mut ir.exprs[index] else {
                    unreachable!()
                };
                args.extend(mask_values);
                args.push(marker);
                *callee = Callee::Static {
                    owner: default.owner,
                    name: default.name.clone(),
                    descriptor: default.descriptor.clone(),
                    // A default bridge for an inline declaration is itself the selected executable
                    // body at this call site. In particular, a non-public/reified bridge must be
                    // spliced; erasing the declaration's inline contract here turns it into an
                    // illegal direct call to a package-part implementation class.
                    inline: callable.inline,
                };
            }
            publish_declared_call_params(
                ir,
                index as crate::ir::ExprId,
                kind,
                member_realization,
                true,
                declared_params,
            );
            publish_inline_modifiers(
                ir,
                expression,
                target,
                kind,
                member_realization,
                true,
                inline_modifiers,
            )?;
            let physical_call =
                bridge_external_result(ir, index, callable.physical_ret, semantic_ret);
            default_call_operands.record(physical_call, operand_plan);
            continue;
        }
        let supplied = match &ir.exprs[index] {
            IrExpr::Call { args, .. } => args.len(),
            _ => unreachable!(),
        };
        // The provider named which physical slots are source parameters. A continuation or a
        // dispatch receiver stored in the vector is not one of them. A `$DefaultImpls` receiver
        // is absent from the vector; the emitter inserts it from `pass_receiver`.
        let source_parameters =
            crate::libraries::physical_parameter_plan::source_physical_parameters(
                &callable.physical_params,
                callable.physical_parameter_plan.as_deref(),
            )
            .map_err(|detail| ExternalRealizationError::Arguments {
                target: target.into(),
                detail,
            })?;
        let source_arity = source_parameters.len();
        let omitted = if supplied == source_arity {
            Vec::new()
        } else if kind == ExternalCallableKind::Extension {
            let receiver = u32::try_from(callable.context_count).map_err(|_| {
                ExternalRealizationError::Arguments {
                    target: target.into(),
                    detail: format!(
                        "call {expression} ({kind:?} {}.{}) extension receiver does not fit a parameter ordinal",
                        callable.physical_owner.render(),
                        physical_name
                    ),
                }
            })?;
            vec![receiver]
        } else {
            return Err(ExternalRealizationError::Arguments {
                target: target.into(),
                detail: format!(
                    "call {expression} ({kind:?} {}.{}) supplies {supplied} arguments for {} physical parameters",
                    callable.physical_owner.render(),
                    physical_name,
                    source_arity
                ),
            });
        };
        super::physical_call_arguments::publish_dependency_parameters(
            ir,
            expression,
            callable.physical_params.clone(),
            callable.physical_parameter_plan.as_deref(),
            supplied,
            &omitted,
        )
        .map_err(|detail| ExternalRealizationError::Arguments {
            target: target.into(),
            detail,
        })?;
        let mut extension_receiver_at = None;
        let IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } = &mut ir.exprs[index]
        else {
            unreachable!()
        };
        let mut physical_result = callable.physical_ret;
        let selected_intrinsic = match callable.compiler_intrinsic {
            Some(crate::libraries::CompilerIntrinsic::StringPlus) => {
                unreachable!("a String.plus call is realized as a concatenation above")
            }
            Some(crate::libraries::CompilerIntrinsic::StringGet) => {
                Some(crate::ir::IrIntrinsic::StringGet)
            }
            Some(crate::libraries::CompilerIntrinsic::NullableAnyToString) => {
                Some(crate::ir::IrIntrinsic::NullableAnyToString)
            }
            Some(crate::libraries::CompilerIntrinsic::EnumValueOf) => {
                Some(crate::ir::IrIntrinsic::EnumValueOf {
                    classifier: semantic_ret,
                })
            }
            // The declaration's only type parameter is the whole operand. Its checked value keeps
            // nullability and projections, which the runtime `KType` has to reproduce.
            Some(crate::libraries::CompilerIntrinsic::TypeOf) => {
                let argument = substitutions.iter().find_map(|substitution| {
                    matches!(
                        substitution.parameter,
                        crate::fir::FirTypeParameterRef::External { callable, ordinal: 0 }
                            if callable == target
                    )
                    .then_some(substitution.value)
                });
                Some(crate::ir::IrIntrinsic::TypeOf {
                    ty: argument.ok_or(target)?,
                })
            }
            Some(
                crate::libraries::CompilerIntrinsic::ArraySize
                | crate::libraries::CompilerIntrinsic::ArrayGet
                | crate::libraries::CompilerIntrinsic::ArraySet
                | crate::libraries::CompilerIntrinsic::ArrayFactory(_)
                | crate::libraries::CompilerIntrinsic::CharCode
                | crate::libraries::CompilerIntrinsic::StringLength
                | crate::libraries::CompilerIntrinsic::Assert
                | crate::libraries::CompilerIntrinsic::AssertFailsWith
                | crate::libraries::CompilerIntrinsic::Print
                | crate::libraries::CompilerIntrinsic::Println
                | crate::libraries::CompilerIntrinsic::StartCoroutine
                | crate::libraries::CompilerIntrinsic::CoroutineContext
                | crate::libraries::CompilerIntrinsic::CoroutineSuspended
                | crate::libraries::CompilerIntrinsic::SuspendCoroutine
                | crate::libraries::CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn
                | crate::libraries::CompilerIntrinsic::EnumValues
                | crate::libraries::CompilerIntrinsic::EnumName
                | crate::libraries::CompilerIntrinsic::IsEmpty
                | crate::libraries::CompilerIntrinsic::IsNotEmpty
                | crate::libraries::CompilerIntrinsic::Count
                | crate::libraries::CompilerIntrinsic::TrimIndent
                | crate::libraries::CompilerIntrinsic::TrimMargin
                | crate::libraries::CompilerIntrinsic::FloatingRangeMembership
                | crate::libraries::CompilerIntrinsic::RangeDownTo
                | crate::libraries::CompilerIntrinsic::RangeUntil
                | crate::libraries::CompilerIntrinsic::ProgressionStep
                | crate::libraries::CompilerIntrinsic::ProgressionReversed
                | crate::libraries::CompilerIntrinsic::UnsignedCompare { .. }
                | crate::libraries::CompilerIntrinsic::PrimitiveIteratorNext
                | crate::libraries::CompilerIntrinsic::NumericConversion
                | crate::libraries::CompilerIntrinsic::PrimitiveUnary(_)
                | crate::libraries::CompilerIntrinsic::PrimitiveCompare
                | crate::libraries::CompilerIntrinsic::PrimitiveBitAnd
                | crate::libraries::CompilerIntrinsic::PrimitiveBitOr
                | crate::libraries::CompilerIntrinsic::PrimitiveBitXor
                | crate::libraries::CompilerIntrinsic::PrimitiveShiftLeft
                | crate::libraries::CompilerIntrinsic::PrimitiveShiftRight
                | crate::libraries::CompilerIntrinsic::PrimitiveUnsignedShiftRight
                | crate::libraries::CompilerIntrinsic::BooleanNot
                | crate::libraries::CompilerIntrinsic::PrimitiveHashCode
                | crate::libraries::CompilerIntrinsic::PrimitiveBitNot
                | crate::libraries::CompilerIntrinsic::PrimitiveBinary(_),
            )
            | None => None,
        };
        if let Some(operation) = selected_intrinsic {
            *callee = Callee::Intrinsic {
                operation,
                ret: semantic_ret,
            };
            physical_result = semantic_ret;
            bridge_external_result(ir, index, physical_result, semantic_ret);
            continue;
        }
        // A member dispatch realizes the dependency declaration the frontend selected.
        let selected = crate::ir::IrVirtualTarget::Function(
            crate::fir::ResolvedFunctionOverrideTarget::External(target),
        );
        match kind {
            ExternalCallableKind::TopLevel => {
                *callee = Callee::Static {
                    owner: callable.physical_owner,
                    name: physical_name.clone(),
                    descriptor,
                    inline: callable.inline,
                };
            }
            ExternalCallableKind::Extension => {
                let receiver = dispatch_receiver.take().ok_or(target)?;
                let position = callable.context_count.min(args.len());
                args.insert(position, receiver);
                extension_receiver_at = Some(position as u32);
                *callee = Callee::Static {
                    owner: callable.physical_owner,
                    name: physical_name.clone(),
                    descriptor,
                    inline: callable.inline,
                };
            }
            ExternalCallableKind::Member => match callable.member_realization {
                crate::libraries::MemberRealization::Dispatch => {
                    // A non-public Kotlin inline member has no legal call instruction: its classfile
                    // method is private and exists only as an inline-body container. Preserve the
                    // dispatch receiver, but route the selected physical handle through the JVM
                    // bytecode splicer. `Callee::Static` is the common IR's existing opaque inline
                    // handle; with a dispatch receiver the emitter prepends `this` to the splice-local
                    // descriptor and emits no static invocation on success. Public inline members keep
                    // ordinary virtual dispatch as their legal fallback.
                    if let crate::ir::IrPropertyDispatch::Super { owner, interface } =
                        property_dispatch
                    {
                        // A legacy interface body lives only on its receiver-first holder static,
                        // which takes the retained dispatch receiver at its own class.
                        *callee = match callable.nonvirtual_realization.as_deref() {
                            Some(holder) => Callee::Static {
                                owner: holder.owner,
                                name: physical_name.clone(),
                                descriptor: holder.descriptor.clone(),
                                inline: crate::libraries::InlineKind::None,
                            },
                            None => Callee::Special {
                                owner,
                                name: physical_name.clone(),
                                descriptor,
                                interface,
                                source_member: None,
                                source: None,
                            },
                        };
                    } else if callable.inline.must_inline() {
                        *callee = Callee::Static {
                            owner: callable.physical_owner,
                            name: physical_name.clone(),
                            descriptor,
                            inline: callable.inline,
                        };
                    } else if let Some((owner, descriptor)) = (semantic_role
                        == Some(crate::libraries::SemanticCallRole::KotlinAnyHashCode))
                    .then(|| ir.ext_call_source_receiver.get(&expression).copied())
                    .flatten()
                    .and_then(primitive_hash_code)
                    {
                        args.insert(0, dispatch_receiver.take().ok_or(target)?);
                        *callee = Callee::Static {
                            owner,
                            name: physical_name.clone(),
                            descriptor,
                            inline: crate::libraries::InlineKind::None,
                        };
                    } else if let Some(array) =
                        crate::jvm::names::array_class_descriptor(callable.physical_owner)
                    {
                        // An array classifier has no JVM class to dispatch on. Its remaining
                        // dispatched member (`iterator()`) is implemented by the unique static
                        // helper its metadata declares for the array's JVM class. The provider
                        // attaches typed intrinsic realizations to `get`/`set`/`size`, so another
                        // selected array dispatch without this helper is invalid backend input.
                        let helper = array_member_helper(
                            classpath,
                            &physical_name,
                            &array,
                            &semantic_params,
                            semantic_ret,
                        )
                        .ok_or(target)?;
                        args.insert(0, dispatch_receiver.take().ok_or(target)?);
                        *callee = Callee::Static {
                            owner: crate::types::type_name(&helper.owner),
                            name: helper.name,
                            descriptor: helper.descriptor,
                            inline: callable.inline,
                        };
                    } else {
                        let (owner, interface) = crate::jvm::member_dispatch::virtual_owner(
                            &dispatch_classifiers,
                            callable.physical_owner,
                            callable.owner_is_interface,
                            ir.dispatch_classes.get(&expression).copied(),
                            ir.ext_call_source_receiver.get(&expression).copied(),
                        )
                        .map_err(|missing| {
                            ExternalDependencyTarget::DispatchClassifier {
                                target,
                                classifier: missing.0,
                            }
                        })?;
                        *callee = Callee::Virtual {
                            owner,
                            name: physical_name.clone(),
                            descriptor,
                            params: None,
                            interface,
                            module_target: None,
                            target: Some(selected),
                        };
                    }
                }
                crate::libraries::MemberRealization::Direct { pass_receiver } => {
                    if pass_receiver {
                        args.insert(0, dispatch_receiver.take().ok_or(target)?);
                    } else {
                        *dispatch_receiver = None;
                    }
                    *callee = Callee::Static {
                        owner: callable.physical_owner,
                        name: physical_name.clone(),
                        descriptor,
                        inline: callable.inline,
                    };
                }
                crate::libraries::MemberRealization::Intrinsic(
                    intrinsic @ (crate::libraries::CompilerIntrinsic::StringGet
                    | crate::libraries::CompilerIntrinsic::NullableAnyToString),
                ) => {
                    physical_result = semantic_ret;
                    *callee = Callee::Intrinsic {
                        operation: if intrinsic == crate::libraries::CompilerIntrinsic::StringGet {
                            crate::ir::IrIntrinsic::StringGet
                        } else {
                            crate::ir::IrIntrinsic::NullableAnyToString
                        },
                        ret: semantic_ret,
                    };
                }
                // An array classifier's own members: the provider attached the operation to the
                // exact `get`/`set`/`size` declaration, so the call becomes that array operation
                // (a `size` getter is called from a property-reference adapter body).
                crate::libraries::MemberRealization::Intrinsic(
                    intrinsic @ (crate::libraries::CompilerIntrinsic::ArrayGet
                    | crate::libraries::CompilerIntrinsic::ArraySet
                    | crate::libraries::CompilerIntrinsic::ArraySize),
                ) => {
                    physical_result = semantic_ret;
                    *callee = Callee::Intrinsic {
                        operation: match intrinsic {
                            crate::libraries::CompilerIntrinsic::ArrayGet => {
                                crate::ir::IrIntrinsic::ArrayGet
                            }
                            crate::libraries::CompilerIntrinsic::ArraySet => {
                                crate::ir::IrIntrinsic::ArraySet
                            }
                            _ => crate::ir::IrIntrinsic::ArraySize,
                        },
                        ret: semantic_ret,
                    };
                }
                crate::libraries::MemberRealization::Intrinsic(
                    crate::libraries::CompilerIntrinsic::PrimitiveIteratorNext,
                ) => {
                    let receiver = ir.ext_call_source_receiver.get(&expression).copied();
                    *callee = match primitive_iterator_next(callable.physical_owner, receiver) {
                        Some((name, element)) => {
                            physical_result = element;
                            Callee::Virtual {
                                owner: callable.physical_owner,
                                name,
                                descriptor: crate::jvm::names::method_descriptor(&[], element),
                                params: None,
                                interface: false,
                                module_target: None,
                                target: Some(selected),
                            }
                        }
                        None => {
                            let (owner, interface) = crate::jvm::member_dispatch::virtual_owner(
                                &dispatch_classifiers,
                                callable.physical_owner,
                                callable.owner_is_interface,
                                ir.dispatch_classes.get(&expression).copied(),
                                ir.ext_call_source_receiver.get(&expression).copied(),
                            )
                            .map_err(|missing| ExternalDependencyTarget::DispatchClassifier {
                                target,
                                classifier: missing.0,
                            })?;
                            Callee::Virtual {
                                owner,
                                name: physical_name.clone(),
                                descriptor,
                                params: None,
                                interface,
                                module_target: None,
                                target: Some(selected),
                            }
                        }
                    };
                }
                crate::libraries::MemberRealization::Intrinsic(
                    crate::libraries::CompilerIntrinsic::PrimitiveHashCode,
                ) => {
                    let receiver = ir.ext_call_source_receiver.get(&expression).copied();
                    *callee = match receiver.and_then(primitive_hash_code) {
                        Some((owner, descriptor)) => {
                            args.insert(0, dispatch_receiver.take().ok_or(target)?);
                            Callee::Static {
                                owner,
                                name: physical_name.clone(),
                                descriptor,
                                inline: crate::libraries::InlineKind::None,
                            }
                        }
                        None => {
                            let (owner, interface) = crate::jvm::member_dispatch::virtual_owner(
                                &dispatch_classifiers,
                                callable.physical_owner,
                                callable.owner_is_interface,
                                ir.dispatch_classes.get(&expression).copied(),
                                ir.ext_call_source_receiver.get(&expression).copied(),
                            )
                            .map_err(|missing| ExternalDependencyTarget::DispatchClassifier {
                                target,
                                classifier: missing.0,
                            })?;
                            Callee::Virtual {
                                owner,
                                name: physical_name.clone(),
                                descriptor,
                                params: None,
                                interface,
                                module_target: None,
                                target: Some(selected),
                            }
                        }
                    };
                }
                crate::libraries::MemberRealization::Intrinsic(_)
                | crate::libraries::MemberRealization::RangeConstruction { .. } => {
                    return Err(target.into());
                }
            },
            ExternalCallableKind::Constructor
            | ExternalCallableKind::InstanceFieldRead
            | ExternalCallableKind::InstanceFieldWrite
            | ExternalCallableKind::StaticFieldRead
            | ExternalCallableKind::StaticFieldWrite => {
                return Err(target.into());
            }
        }
        if extension_receiver_at.is_none() && kind == ExternalCallableKind::Member {
            if let Some(position) = extension_receiver_parameter {
                let consumes_dispatch = matches!(
                    member_realization,
                    crate::libraries::MemberRealization::Direct {
                        pass_receiver: true
                    }
                );
                extension_receiver_at = Some(position + u32::from(consumes_dispatch));
            }
        }
        if let Some(position) = extension_receiver_at {
            ir.static_extension_receivers.insert(expression, position);
        }
        publish_declared_call_params(
            ir,
            index as crate::ir::ExprId,
            kind,
            member_realization,
            false,
            declared_params,
        );
        publish_inline_modifiers(
            ir,
            expression,
            target,
            kind,
            member_realization,
            false,
            inline_modifiers,
        )?;
        let realized_call = bridge_external_result(ir, index, physical_result, semantic_ret);
        if let Some(role) = semantic_role {
            ir.semantic_call_roles.insert(realized_call, role);
        }
    }
    for (owner, target) in &mut ir.external_super_constructors {
        let uses_defaults = ir
            .super_constructor_default_arguments
            .get(owner)
            .is_some_and(|parameters| !parameters.is_empty());
        target.descriptor = Some(external_constructor_descriptor(
            callables,
            target.declaration,
            uses_defaults,
        )?);
    }
    for ((owner, ordinal), target) in &mut ir.external_secondary_super_constructors {
        let constructor = ir
            .classes
            .iter()
            .find(|class| class.fq_name == *owner)
            .and_then(|class| class.secondary_ctors.get(*ordinal as usize))
            .ok_or(target.declaration)?;
        target.descriptor = Some(external_constructor_descriptor(
            callables,
            target.declaration,
            !constructor.default_parameters.is_empty(),
        )?);
    }
    Ok(())
}

/// The wrapper's static `hashCode` and its descriptor that hash a non-null scalar receiver
/// (`Integer.hashCode(I)I`), as kotlinc's `HashCode` intrinsic calls it for a JVM 1.8+ target.
/// A receiver without a primitive representation keeps the ordinary virtual `hashCode()`.
fn primitive_hash_code(receiver: crate::types::Ty) -> Option<(crate::types::TypeName, String)> {
    let scalar = receiver.scalar_value_repr()?;
    if receiver.is_nullable() || scalar != receiver || receiver.is_unsigned() {
        return None;
    }
    let wrapper = crate::jvm::jvm_class_map::wrapper_type_name(receiver)?;
    let descriptor = crate::jvm::names::method_descriptor(&[scalar], crate::types::Ty::Int);
    Some((wrapper, descriptor))
}

/// The unboxed element operation a primitive iterator's `next()` is invoked through, with the
/// element type it returns.
///
/// kotlinc's `IteratorNext` intrinsic calls `IntIterator.nextInt()I` when the selected declaration is
/// the primitive iterator's own `next`, i.e. the receiver's checked type is that iterator class. A
/// receiver of a subclass selects the subclass's inherited member, which kotlinc calls as the
/// ordinary boxed `next()`, so `None` then.
fn primitive_iterator_next(
    iterator: crate::types::TypeName,
    receiver: Option<crate::types::Ty>,
) -> Option<(String, crate::types::Ty)> {
    use crate::types::Ty;
    let element = crate::types::wk::primitive_iterator_element(iterator)?;
    if receiver.and_then(|receiver| receiver.non_null().obj_internal()) != Some(iterator) {
        return None;
    }
    let element_name = match element {
        Ty::Boolean => "Boolean",
        Ty::Byte => "Byte",
        Ty::Char => "Char",
        Ty::Short => "Short",
        Ty::Int => "Int",
        Ty::Long => "Long",
        Ty::Float => "Float",
        Ty::Double => "Double",
        _ => return None,
    };
    Some((format!("next{element_name}"), element))
}

/// The static helper implementing a dispatched member of an array classifier, keyed by the
/// member's declared name and its signature on the JVM array class (`iterator([I)` for
/// `IntArray.iterator()`). A reference array's element is erased, so every `Array<T>` shares the
/// `[Ljava/lang/Object;` form; any other array-typed operand is erased the same way.
fn array_member_helper(
    classpath: &Classpath,
    name: &str,
    array: &str,
    parameters: &[crate::types::Ty],
    result: crate::types::Ty,
) -> Option<crate::jvm::inline::StaticMemberRealization> {
    let erased = |ty: crate::types::Ty| {
        ty.obj_internal()
            .filter(|_| ty.is_array())
            .and_then(crate::jvm::names::array_class_descriptor)
            .unwrap_or_else(|| crate::jvm::names::type_descriptor(ty))
    };
    let mut descriptor = format!("({array}");
    for parameter in parameters {
        descriptor.push_str(&erased(*parameter));
    }
    descriptor.push(')');
    descriptor.push_str(&erased(result));
    classpath.static_array_member_realization(name, &descriptor)
}

fn external_constructor_descriptor(
    callables: &crate::backend::CheckedBackendCallables,
    target: ExternalCallableId,
    uses_defaults: bool,
) -> Result<String, ExternalCallableId> {
    let callable = callables.callable(target).ok_or(target)?;
    if callable.kind != ExternalCallableKind::Constructor {
        return Err(target);
    }
    if uses_defaults {
        let default = callable.default_realization.as_deref().ok_or(target)?;
        if default.name != "<init>" || default.mask_count == 0 {
            return Err(target);
        }
        return Ok(default.descriptor.clone());
    }
    Ok(if callable.descriptor.is_empty() {
        crate::jvm::names::method_descriptor(&callable.physical_params, crate::types::Ty::Unit)
    } else {
        callable.descriptor.clone()
    })
}

/// Translate checked provider-parameter ordinals into the metadata names consumed by the JVM
/// bytecode inliner. The values and declaration identity were fixed in FIR; this backend step only
/// exposes the physical formal spelling carried by the selected dependency declaration.
fn publish_reified_substitutions(
    ir: &mut IrFile,
    expression: crate::ir::ExprId,
    target: ExternalCallableId,
    callable: &crate::backend::BackendCallableFact,
    substitutions: &[crate::ir::IrCheckedSubstitution],
) {
    if !callable.inline.can_inline() {
        return;
    }
    let Some(signature) = callable.generic_sig.as_deref() else {
        return;
    };
    let bindings = substitutions
        .iter()
        .filter(|substitution| substitution.reified)
        .filter_map(|substitution| {
            let crate::fir::FirTypeParameterRef::External {
                callable: declaration,
                ordinal,
            } = substitution.parameter
            else {
                return None;
            };
            if declaration != target {
                return None;
            }
            signature
                .formals
                .get(ordinal as usize)
                .map(|name| (name.clone(), substitution.value))
        })
        .collect::<Vec<_>>();
    if !bindings.is_empty() {
        ir.reified_call_subst.insert(expression, bindings);
    }
}

/// The provider owns physical erasure; checked FIR owns the final semantic result. Preserve both by
/// wrapping the realized physical call at its original expression identity.
pub(super) fn bridge_external_result(
    ir: &mut IrFile,
    index: usize,
    physical: crate::types::Ty,
    semantic: crate::types::Ty,
) -> crate::ir::ExprId {
    if physical == semantic {
        return index as crate::ir::ExprId;
    }
    let call = ir.exprs[index].clone();
    let call = ir.add_expr(call);
    copy_call_facts(ir, index as crate::ir::ExprId, call);
    // Realization itself is the authoritative boundary between the checked Kotlin result and the
    // provider's physical result. Do not depend on an incidental pre-existing side-table entry:
    // value-class lowering needs both facts on the cloned operation to distinguish an erased generic
    // read (`Iterator<X>.next(): Object`, which returns a boxed `X`) from a declaration whose value-
    // class result is already returned as its unboxed carrier.
    ir.logical_types.insert(call, semantic);
    ir.physical_types.insert(call, physical);
    ir.exprs[index] = IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::ImplicitCoercion,
        arg: call,
        type_operand: semantic,
    };
    call
}

/// Attach the selected declaration's parameter shape to the realized call. The provider shape omits
/// an ordinary dispatch receiver; static member realizations that consume it prepend the receiver type
/// already recorded by checked lowering. Default stubs do the same independently of the member's normal
/// realization. Mask/marker suffixes deliberately have no semantic entries and fall back to their JVM
/// descriptor slots in the representation pass.
fn publish_declared_call_params(
    ir: &mut IrFile,
    expression: crate::ir::ExprId,
    kind: ExternalCallableKind,
    member_realization: crate::libraries::MemberRealization,
    default_call: bool,
    declared: Option<Box<[crate::types::Ty]>>,
) {
    let Some(declared) = declared else {
        return;
    };
    let mut declared = declared.into_vec();
    let consumes_dispatch = kind == ExternalCallableKind::Member
        && (default_call
            || matches!(
                member_realization,
                crate::libraries::MemberRealization::Direct {
                    pass_receiver: true
                }
            ));
    if consumes_dispatch {
        let Some(receiver) = ir.ext_call_source_receiver.get(&expression).copied() else {
            return;
        };
        declared.insert(0, receiver);
    }
    let argument_count = match ir.expr(expression) {
        IrExpr::Call { args, .. } => args.len(),
        _ => return,
    };
    if declared.len() <= argument_count {
        ir.call_declared_params
            .insert(expression, declared.into_boxed_slice());
    }
}

/// Align the provider-published `crossinline`/`noinline` modifiers with the realized call operands.
/// Kotlin metadata excludes receiver operands; realization inserts explicit unmodified entries and
/// pads only the backend-owned default-mask/marker suffix. Inconsistent declaration data rejects
/// the selected target rather than silently assigning a role to the wrong operand.
fn publish_inline_modifiers(
    ir: &mut IrFile,
    expression: crate::ir::ExprId,
    target: ExternalCallableId,
    kind: ExternalCallableKind,
    member_realization: crate::libraries::MemberRealization,
    default_call: bool,
    roles: Box<[InlineParameterModifier]>,
) -> Result<(), ExternalDependencyTarget> {
    if roles.is_empty() {
        return Ok(());
    }
    let consumes_dispatch = kind == ExternalCallableKind::Member
        && (default_call
            || matches!(
                member_realization,
                crate::libraries::MemberRealization::Direct {
                    pass_receiver: true
                }
            ));
    let extension_receiver = ir
        .static_extension_receivers
        .get(&expression)
        .copied()
        .map(|position| position as usize);
    let argument_count = match ir.expr(expression) {
        IrExpr::Call { args, .. } => args.len(),
        _ => return Err(target.into()),
    };
    let roles = align_inline_modifiers(
        roles,
        consumes_dispatch,
        extension_receiver,
        argument_count,
        default_call,
    )
    .ok_or(target)?;
    ir.call_inline_modifiers.insert(expression, roles);
    Ok(())
}

fn align_inline_modifiers(
    roles: Box<[InlineParameterModifier]>,
    consumes_dispatch: bool,
    extension_receiver: Option<usize>,
    argument_count: usize,
    default_call: bool,
) -> Option<Box<[InlineParameterModifier]>> {
    let mut roles = roles.into_vec();
    if consumes_dispatch {
        roles.insert(0, InlineParameterModifier::None);
    }
    if let Some(position) = extension_receiver {
        if position > roles.len() {
            return None;
        }
        roles.insert(position, InlineParameterModifier::None);
    }
    if roles.len() > argument_count {
        return None;
    }
    // Only a `$default` bridge owns extra physical operands: its mask words and marker. An
    // ordinary call must align exactly, otherwise padding would silently assign no modifier to a
    // declaration parameter whose metadata was missing or misaligned.
    if !default_call && roles.len() != argument_count {
        return None;
    }
    roles.resize(argument_count, InlineParameterModifier::None);
    Some(roles.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::align_inline_modifiers;
    use crate::types::InlineParameterModifier as Modifier;

    #[test]
    fn inline_modifiers_follow_realized_receiver_and_default_operands() {
        assert_eq!(
            align_inline_modifiers(
                vec![Modifier::None, Modifier::Crossinline].into_boxed_slice(),
                true,
                Some(2),
                6,
                true,
            )
            .as_deref(),
            Some(
                [
                    Modifier::None,
                    Modifier::None,
                    Modifier::None,
                    Modifier::Crossinline,
                    Modifier::None,
                    Modifier::None
                ]
                .as_slice()
            ),
        );
    }

    #[test]
    fn inconsistent_inline_modifiers_are_rejected() {
        assert_eq!(
            align_inline_modifiers(
                vec![Modifier::Noinline].into_boxed_slice(),
                false,
                Some(2),
                2,
                false,
            ),
            None,
        );
        assert_eq!(
            align_inline_modifiers(
                vec![Modifier::None, Modifier::Noinline].into_boxed_slice(),
                true,
                None,
                2,
                false,
            ),
            None,
        );
        assert_eq!(
            align_inline_modifiers(
                vec![Modifier::Noinline].into_boxed_slice(),
                false,
                None,
                2,
                false,
            ),
            None,
        );
    }
}
