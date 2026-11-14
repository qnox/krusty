use krusty::fir::ExternalCallableId;
use krusty::ir::{Callee, IrCheckedOperation, IrExpr, IrFile};
use krusty::jvm::classpath::{Classpath, ExternalCallableKind};
use krusty::types::{InlineParameterModifier, Ty};

use super::default_call_operands::{DefaultCallOperand, DefaultCallOperands};

mod arguments;
pub(super) use arguments::ExternalRealizationError;
use arguments::{
    copy_call_facts, materialize_constructor_defaults, materialize_omitted_arguments,
    ExternalDependencyTarget,
};

pub(super) fn realize(
    ir: &mut IrFile,
    classifiers: &dyn krusty::backend::BackendClassifierSource,
    classpath: &Classpath,
    callables: &krusty::backend::CheckedBackendCallables,
    default_call_operands: &mut DefaultCallOperands,
) -> Result<(), ExternalRealizationError> {
    realize_expressions(
        ir,
        classifiers,
        classpath,
        callables,
        default_call_operands,
        Realized::File,
    )
}

/// Realize the calls native plugins generated, from expression `first` on, to the runtime
/// declarations they selected at the handoff, and the file's calls to declarations a plugin's
/// transform appended parameters to, which the plugin has now passed. The file's other dependency
/// calls were realized at lowering; any other expression a plugin appended keeps its own
/// realization path.
pub(super) fn realize_plugin_runtime_calls(
    ir: &mut IrFile,
    classifiers: &dyn krusty::backend::BackendClassifierSource,
    classpath: &Classpath,
    callables: &krusty::backend::CheckedBackendCallables,
    default_call_operands: &mut DefaultCallOperands,
    first: usize,
) -> Result<(), ExternalRealizationError> {
    realize_expressions(
        ir,
        classifiers,
        classpath,
        callables,
        default_call_operands,
        Realized::PluginRuntimeCalls { first },
    )
}

/// Which expressions one realization pass rewrites.
#[derive(Clone, Copy)]
enum Realized {
    /// Every dependency call and construction of the file.
    File,
    /// The calls to plugin-selected runtime declarations among the expressions from `first` on,
    /// and every call to a declaration with appended parameters.
    PluginRuntimeCalls { first: usize },
}

impl Realized {
    /// Whether this pass realizes `expression`, the one at `index`. A call to a declaration a
    /// plugin's transform appended parameters to waits for the plugin pass, which runs after the
    /// plugin has passed them.
    fn includes(
        self,
        index: usize,
        expression: &IrExpr,
        callables: &krusty::backend::CheckedBackendCallables,
    ) -> bool {
        let target = match expression {
            IrExpr::Call { callee, .. } => match **callee {
                Callee::External { target, .. } => Some(target),
                _ => None,
            },
            _ => None,
        };
        let appended =
            target.is_some_and(|target| !callables.plugin_runtime_appended(target).is_empty());
        match self {
            Self::File => !appended,
            Self::PluginRuntimeCalls { first } => {
                appended
                    || (index >= first
                        && target
                            .is_some_and(|target| callables.is_plugin_runtime_callable(target)))
            }
        }
    }
}

fn realize_expressions(
    ir: &mut IrFile,
    classifiers: &dyn krusty::backend::BackendClassifierSource,
    classpath: &Classpath,
    callables: &krusty::backend::CheckedBackendCallables,
    default_call_operands: &mut DefaultCallOperands,
    realized: Realized,
) -> Result<(), ExternalRealizationError> {
    // Body-local, anonymous, and backend-generated receivers are complete only in this file's
    // common IR. Freeze them before mutating call nodes and combine them with the stable
    // module/dependency snapshot for every dispatch-owner decision below.
    let dispatch_classifiers =
        crate::member_dispatch::CheckedDispatchClassifiers::new(ir, classifiers);
    let expression_count = ir.exprs.len();
    let mut realized_calls = Vec::new();
    for index in 0..expression_count {
        if !realized.includes(index, &ir.exprs[index], callables) {
            continue;
        }
        let expression = u32::try_from(index).expect("too many common IR expressions");
        realized_calls.push(expression);
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
                krusty::trace_compiler!(
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
                krusty::libraries::MemberRealization::Direct {
                    pass_receiver: false
                }
            ) && physical_name != "<init>"
            {
                let (owner, name, descriptor, real_params, suffix) = if defaults.is_empty() {
                    (
                        callable.physical_owner,
                        physical_name,
                        if callable.descriptor.is_empty() {
                            krusty::jvm::names::method_descriptor(
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
                        .map(|mask| ir.add_expr(IrExpr::Const(krusty::ir::IrConst::Int(mask))))
                        .collect::<Vec<_>>();
                    suffix.push(ir.add_expr(IrExpr::Const(krusty::ir::IrConst::Null)));
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
                    callee: Box::new(Callee::Static {
                        owner,
                        name,
                        descriptor,
                        inline: callable.inline,
                    }),
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
                    .map(|mask| ir.add_expr(IrExpr::Const(krusty::ir::IrConst::Int(mask))))
                    .collect::<Vec<_>>();
                operands.push(ir.add_expr(IrExpr::Const(krusty::ir::IrConst::Null)));
                operands
            } else if constructor.is_some() {
                vec![ir.add_expr(IrExpr::Const(krusty::ir::IrConst::Null))]
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
            if default.is_some() {
                // The placeholders, masks and marker belong to the construction, as a defaulted
                // call's do: emission puts the construction's line back at each run of them.
                let operand_plan = realized_arguments
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(position, argument)| {
                        let parameter = (position as u32).checked_sub(default_prefix_count);
                        match parameter.filter(|parameter| defaults.contains(parameter)) {
                            Some(parameter) => {
                                DefaultCallOperand::synthesized_default(argument, parameter)
                            }
                            None => DefaultCallOperand::supplied(argument),
                        }
                    })
                    .chain(
                        realization_operands
                            .iter()
                            .copied()
                            .map(DefaultCallOperand::synthesized_abi),
                    )
                    .collect();
                default_call_operands.record(expression, operand_plan);
            }
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
            *ctor_desc = Some(Box::from(if let Some(default) = default {
                default.descriptor.clone()
            } else if let Some(constructor) = constructor {
                constructor.descriptor.clone()
            } else if callable.descriptor.is_empty() {
                krusty::jvm::names::method_descriptor(
                    &callable.physical_params,
                    krusty::types::Ty::Unit,
                )
            } else {
                callable.descriptor
            }));
            *external_target = None;
            *defaults = Box::new([]);
            *default_prefix_count = 0;
            continue;
        }
        let checked_expression = ir.exprs[index].clone();
        let property = match checked_expression {
            IrExpr::Checked(ref checked_op)
                if let IrCheckedOperation::ExternalPropertyRead {
                    target,
                    dispatch,
                    receiver,
                    arguments,
                    parameters,
                    result,
                    substitutions,
                    source_receiver,
                } = &**checked_op =>
            {
                Some((
                    target,
                    false,
                    dispatch,
                    receiver,
                    arguments,
                    parameters,
                    result,
                    substitutions,
                    source_receiver,
                ))
            }
            IrExpr::Checked(ref checked_op)
                if let IrCheckedOperation::ExternalPropertyWrite {
                    target,
                    dispatch,
                    receiver,
                    arguments,
                    parameters,
                    result,
                    substitutions,
                    source_receiver,
                } = &**checked_op =>
            {
                Some((
                    target,
                    true,
                    dispatch,
                    receiver,
                    arguments,
                    parameters,
                    result,
                    substitutions,
                    source_receiver,
                ))
            }
            _ => None,
        };
        let mut property_dispatch = krusty::ir::IrPropertyDispatch::Ordinary;
        let mut property_virtual_target = None;
        if let Some((
            property,
            write,
            dispatch,
            receiver,
            arguments,
            parameters,
            result,
            substitutions,
            source_receiver,
        )) = property
        {
            property_dispatch = *dispatch;
            let realization = callables
                .property(*property)
                .ok_or(ExternalDependencyTarget::Property(*property))?;
            let target = if write {
                realization
                    .setter
                    .ok_or(ExternalDependencyTarget::Property(*property))?
            } else {
                realization.getter
            };
            let accessor = callables
                .callable(target)
                .ok_or(ExternalDependencyTarget::Callable(target))?;
            let extension_receiver_parameter = if accessor.is_member_extension()
                || (accessor.is_extension() && receiver.is_none())
            {
                Some(
                    u32::try_from(accessor.context_count)
                        .map_err(|_| ExternalDependencyTarget::Callable(target))?,
                )
            } else {
                None
            };
            let property_target = krusty::fir::ResolvedPropertyOverrideTarget::External(target);
            property_virtual_target = Some(if write {
                krusty::ir::IrVirtualTarget::PropertySetter(property_target)
            } else {
                krusty::ir::IrVirtualTarget::PropertyGetter(property_target)
            });
            ir.exprs[index] = IrExpr::Call {
                callee: Box::new(Callee::External {
                    target,
                    default_provider: None,
                    params: parameters.to_vec(),
                    ret: *result,
                    substitutions: substitutions.to_vec(),
                    defaults: Vec::new(),
                    extension_receiver_parameter,
                }),
                dispatch_receiver: *receiver,
                args: arguments.to_vec(),
            };
            if let Some(source_receiver) = source_receiver {
                ir.ext_call_source_receiver
                    .insert(expression, *source_receiver);
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
                callee: call_callee,
                ..
            } if let Callee::External {
                target,
                default_provider,
                params,
                ret,
                substitutions,
                defaults,
                extension_receiver_parameter,
            } = &**call_callee =>
            {
                (
                    *target,
                    *default_provider,
                    params.clone(),
                    *ret,
                    substitutions.clone(),
                    defaults.clone(),
                    *extension_receiver_parameter,
                )
            }
            _ => continue,
        };
        let callable = callables.callable(target).cloned().ok_or_else(|| {
            krusty::trace_compiler!(
                "fir",
                "missing external call realization id={target:?} expression={index} semantic_result={semantic_ret:?}"
            );
            target
        })?;
        let physical_name = callable.physical_name().to_string();
        let kind = callable.kind;
        publish_inline_substitutions(ir, expression, target, &callable, &substitutions);
        let declared_params = callable.declared_params.clone();
        let mut inline_modifiers = callable.inline_modifiers.clone();
        let member_realization = callable.member_realization;
        let semantic_role = callable.semantic_role;
        let descriptor = if callable.descriptor.is_empty() {
            krusty::jvm::names::method_descriptor(&callable.physical_params, callable.physical_ret)
        } else {
            callable.descriptor.clone()
        };
        // A `String.plus` that reaches this boundary is a callable-reference adapter body (source
        // calls lower as concatenations): it is the same two-part concatenation.
        if callable.compiler_intrinsic == Some(krusty::libraries::CompilerIntrinsic::StringPlus)
            || member_realization
                == krusty::libraries::MemberRealization::Intrinsic(
                    krusty::libraries::CompilerIntrinsic::StringPlus,
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
        if let krusty::libraries::MemberRealization::Intrinsic(intrinsic) = member_realization {
            if !matches!(
                intrinsic,
                krusty::libraries::CompilerIntrinsic::StringGet
                    | krusty::libraries::CompilerIntrinsic::ArrayGet
                    | krusty::libraries::CompilerIntrinsic::ArraySet
                    | krusty::libraries::CompilerIntrinsic::ArraySize
                    | krusty::libraries::CompilerIntrinsic::PrimitiveIteratorNext
                    | krusty::libraries::CompilerIntrinsic::NullableAnyToString
                    | krusty::libraries::CompilerIntrinsic::PrimitiveHashCode
                    | krusty::libraries::CompilerIntrinsic::PrimitiveEquals
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
                // The selected declaration owns the intrinsic's result. A callable-reference
                // adapter may expose its erased carrier at this call site, but that must not erase
                // the operation's semantic value-class identity.
                let result = callable.ret;
                let operation = super::builtin_member_operations::operation(
                    ir,
                    intrinsic,
                    super::builtin_member_operations::BuiltinMemberOperands {
                        receiver,
                        receiver_ty,
                        arguments: &arguments,
                        parameters: &semantic_params,
                        result,
                    },
                )
                .ok_or(target)?;
                ir.ext_call_source_receiver.remove(&expression);
                operation.commit(ir, expression);
                if matches!(result.canonical_semantic().non_null(), Ty::UInt | Ty::ULong) {
                    ir.set_logical_type(expression, result);
                }
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
            let operation = Some(index as krusty::ir::ExprId);
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
            krusty::trace_compiler!(
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
            let source_plan = krusty::libraries::physical_parameter_plan::source_parameter_plan(
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
            let member_extension_receiver = if member_extension {
                Some(extension_receiver.ok_or(target)?)
            } else {
                None
            };
            let mut operand_plan = realized_arguments
                .iter()
                .copied()
                .enumerate()
                .map(|(parameter, argument)| {
                    let source_parameter = if let Some(receiver) = member_extension_receiver {
                        match parameter.cmp(&receiver) {
                            std::cmp::Ordering::Less => Some(parameter as u32),
                            std::cmp::Ordering::Equal => None,
                            std::cmp::Ordering::Greater => Some((parameter - 1) as u32),
                        }
                    } else {
                        Some(parameter as u32)
                    };
                    if let Some(omitted) = omitted_parameters
                        .iter()
                        .position(|&physical| physical == parameter as u32)
                    {
                        DefaultCallOperand::synthesized_default(argument, defaults[omitted])
                    } else if let Some(source_parameter) = source_parameter {
                        DefaultCallOperand::supplied_parameter(argument, source_parameter)
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
                .map(|mask| ir.add_expr(IrExpr::Const(krusty::ir::IrConst::Int(mask))))
                .collect::<Vec<_>>();
            let marker = ir.add_expr(IrExpr::Const(krusty::ir::IrConst::Null));
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
                **callee = Callee::Static {
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
                index as krusty::ir::ExprId,
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
            // The invoked operation is the provider's `$default` bridge, so its descriptor result is
            // the physical value on the stack. An inherited provider can erase a generic result
            // (`Base<T>.f$default(...): Object`) that the selected override returns as `int`.
            let physical_call = bridge_external_result(ir, index, default.ret, semantic_ret);
            default_call_operands.record(physical_call, operand_plan);
            default_call_operands.record_declaration(
                physical_call,
                crate::default_call_operands::DefaultedDeclaration {
                    owner: callable.physical_owner,
                    name: physical_name.clone(),
                    descriptor: if callable.descriptor.is_empty() {
                        krusty::jvm::names::method_descriptor(
                            &callable.physical_params,
                            callable.physical_ret,
                        )
                    } else {
                        callable.descriptor.clone()
                    },
                },
            );
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
            krusty::libraries::physical_parameter_plan::source_physical_parameters(
                &callable.physical_params,
                callable.physical_parameter_plan.as_deref(),
            )
            .map_err(|detail| ExternalRealizationError::Arguments {
                target: target.into(),
                detail,
            })?;
        let source_arity = source_parameters.len();
        // A plugin's transform appended parameters to a runtime declaration it compiled (Compose's
        // `$composer` and `$changed`); its generated calls pass them after the declared ones, and
        // the compiled method takes them last.
        let appended = callables.plugin_runtime_appended(target);
        if !appended.is_empty() {
            let physical = appended_physical_parameters(&source_parameters, appended, &descriptor)
                .map_err(|detail| ExternalRealizationError::Arguments {
                    target: target.into(),
                    detail: format!(
                        "call {expression} ({kind:?} {}.{}) {detail}",
                        callable.physical_owner.render(),
                        physical_name
                    ),
                })?;
            // The appended operands are ordinary values: no `crossinline` or `noinline` role.
            if !inline_modifiers.is_empty() {
                inline_modifiers = inline_modifiers
                    .iter()
                    .copied()
                    .chain(appended.iter().map(|_| InlineParameterModifier::None))
                    .collect();
            }
            let plan =
                krusty::libraries::physical_parameter_plan::source_parameter_plan(physical.len());
            super::physical_call_arguments::publish_dependency_parameters(
                ir,
                expression,
                physical,
                Some(&plan),
                supplied,
                &[],
            )
            .map_err(|detail| ExternalRealizationError::Arguments {
                target: target.into(),
                detail,
            })?;
        }
        let omitted = if !appended.is_empty() || supplied == source_arity {
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
        if appended.is_empty() {
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
        }
        let mut extension_receiver_at = None;
        let annotation_receiver = match &ir.exprs[index] {
            IrExpr::Call {
                dispatch_receiver: Some(receiver),
                ..
            } => super::annotation_constructions::construction_interface(ir, *receiver),
            _ => None,
        };
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
            Some(krusty::libraries::CompilerIntrinsic::StringPlus) => {
                unreachable!("a String.plus call is realized as a concatenation above")
            }
            Some(krusty::libraries::CompilerIntrinsic::StringGet) => {
                Some(krusty::ir::IrIntrinsic::StringGet)
            }
            Some(krusty::libraries::CompilerIntrinsic::NullableAnyToString) => {
                Some(krusty::ir::IrIntrinsic::NullableAnyToString)
            }
            Some(krusty::libraries::CompilerIntrinsic::EnumValueOf) => {
                Some(krusty::ir::IrIntrinsic::EnumValueOf {
                    classifier: semantic_ret,
                })
            }
            Some(krusty::libraries::CompilerIntrinsic::EnumEntries) => {
                let classifier = substitutions.iter().find_map(|substitution| {
                    matches!(
                        substitution.parameter,
                        krusty::fir::FirTypeParameterRef::External { callable, ordinal: 0 }
                            if callable == target
                    )
                    .then_some(substitution.value)
                });
                Some(krusty::ir::IrIntrinsic::EnumEntries {
                    classifier: classifier.ok_or(target)?,
                })
            }
            // The declaration's only type parameter is the whole operand. Its checked value keeps
            // nullability and projections, which the runtime `KType` has to reproduce.
            Some(krusty::libraries::CompilerIntrinsic::TypeOf) => {
                let argument = substitutions.iter().find_map(|substitution| {
                    matches!(
                        substitution.parameter,
                        krusty::fir::FirTypeParameterRef::External { callable, ordinal: 0 }
                            if callable == target
                    )
                    .then_some(substitution.value)
                });
                Some(krusty::ir::IrIntrinsic::TypeOf {
                    ty: argument.ok_or(target)?,
                })
            }
            Some(krusty::libraries::CompilerIntrinsic::ReinterpretBits) => {
                unreachable!("target KLIB reinterpretation cannot reach JVM call realization")
            }
            Some(krusty::libraries::CompilerIntrinsic::ScalarInstruction(_)) => {
                unreachable!("a target KLIB scalar instruction cannot reach JVM call realization")
            }
            Some(
                krusty::libraries::CompilerIntrinsic::UnsignedDivide { .. }
                | krusty::libraries::CompilerIntrinsic::UnsignedRemainder { .. }
                | krusty::libraries::CompilerIntrinsic::ResultFailure(_)
                | krusty::libraries::CompilerIntrinsic::TextTrim
                | krusty::libraries::CompilerIntrinsic::CollectionSizeOrDefault,
            ) => {
                unreachable!("a target KLIB library helper cannot reach JVM call realization")
            }
            Some(
                krusty::libraries::CompilerIntrinsic::ThrowableConstructor { .. }
                | krusty::libraries::CompilerIntrinsic::ThrowableMessage
                | krusty::libraries::CompilerIntrinsic::ThrowableCause,
            ) => unreachable!("target KLIB Throwable storage cannot reach JVM call realization"),
            // The JVM stdlib implements these as `KClass.getSimpleName` and
            // `getQualifiedName`. A target that builds its own class tokens answers the same
            // intrinsic from those tokens; that token is not a JVM callee.
            Some(krusty::libraries::CompilerIntrinsic::ClassName(_)) => None,
            Some(
                krusty::libraries::CompilerIntrinsic::ArraySize
                | krusty::libraries::CompilerIntrinsic::FloatingIsNaN { .. }
                | krusty::libraries::CompilerIntrinsic::FloatingIsInfinite { .. }
                | krusty::libraries::CompilerIntrinsic::RadixToString { .. }
                | krusty::libraries::CompilerIntrinsic::ArrayGet
                | krusty::libraries::CompilerIntrinsic::ArraySet
                | krusty::libraries::CompilerIntrinsic::ArrayFactory(_)
                | krusty::libraries::CompilerIntrinsic::ListStorageConstruction(_)
                | krusty::libraries::CompilerIntrinsic::CharCode
                | krusty::libraries::CompilerIntrinsic::StringLength
                | krusty::libraries::CompilerIntrinsic::Assert
                | krusty::libraries::CompilerIntrinsic::AssertFailsWith
                | krusty::libraries::CompilerIntrinsic::Print
                | krusty::libraries::CompilerIntrinsic::Println
                | krusty::libraries::CompilerIntrinsic::StartCoroutine
                | krusty::libraries::CompilerIntrinsic::CoroutineContext
                | krusty::libraries::CompilerIntrinsic::CoroutineSuspended
                | krusty::libraries::CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn
                | krusty::libraries::CompilerIntrinsic::EnumValues
                | krusty::libraries::CompilerIntrinsic::EnumName
                | krusty::libraries::CompilerIntrinsic::IsEmpty
                | krusty::libraries::CompilerIntrinsic::IsNotEmpty
                | krusty::libraries::CompilerIntrinsic::Count
                | krusty::libraries::CompilerIntrinsic::TrimIndent
                | krusty::libraries::CompilerIntrinsic::TrimMargin
                | krusty::libraries::CompilerIntrinsic::FloatingRangeMembership
                | krusty::libraries::CompilerIntrinsic::ClosedRangeContains
                | krusty::libraries::CompilerIntrinsic::ComparableRangeTo
                | krusty::libraries::CompilerIntrinsic::RangeDownTo
                | krusty::libraries::CompilerIntrinsic::RangeUntil
                | krusty::libraries::CompilerIntrinsic::ProgressionStep
                | krusty::libraries::CompilerIntrinsic::ProgressionReversed
                | krusty::libraries::CompilerIntrinsic::WithIndex
                | krusty::libraries::CompilerIntrinsic::Indices(_)
                | krusty::libraries::CompilerIntrinsic::UnsignedCompare { .. }
                | krusty::libraries::CompilerIntrinsic::PrimitiveIteratorNext
                | krusty::libraries::CompilerIntrinsic::ResultFailureException
                | krusty::libraries::CompilerIntrinsic::NumericConversion
                | krusty::libraries::CompilerIntrinsic::PrimitiveUnary(_)
                | krusty::libraries::CompilerIntrinsic::PrimitiveCompare
                | krusty::libraries::CompilerIntrinsic::PrimitiveBitAnd
                | krusty::libraries::CompilerIntrinsic::PrimitiveBitOr
                | krusty::libraries::CompilerIntrinsic::PrimitiveBitXor
                | krusty::libraries::CompilerIntrinsic::PrimitiveShiftLeft
                | krusty::libraries::CompilerIntrinsic::PrimitiveShiftRight
                | krusty::libraries::CompilerIntrinsic::PrimitiveUnsignedShiftRight
                | krusty::libraries::CompilerIntrinsic::BooleanNot
                | krusty::libraries::CompilerIntrinsic::PrimitiveHashCode
                | krusty::libraries::CompilerIntrinsic::PrimitiveEquals
                | krusty::libraries::CompilerIntrinsic::PrimitiveBitNot
                | krusty::libraries::CompilerIntrinsic::PrimitiveBinary(_),
            )
            | None => None,
        };
        if let Some(operation) = selected_intrinsic {
            **callee = Callee::Intrinsic {
                operation,
                ret: semantic_ret,
            };
            physical_result = semantic_ret;
            bridge_external_result(ir, index, physical_result, semantic_ret);
            continue;
        }
        // A member dispatch realizes the dependency declaration the frontend selected.
        let selected = property_virtual_target.unwrap_or({
            krusty::ir::IrVirtualTarget::Function(
                krusty::fir::ResolvedFunctionOverrideTarget::External(target),
            )
        });
        match kind {
            ExternalCallableKind::TopLevel => {
                **callee = Callee::Static {
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
                **callee = Callee::Static {
                    owner: callable.physical_owner,
                    name: physical_name.clone(),
                    descriptor,
                    inline: callable.inline,
                };
            }
            ExternalCallableKind::Member => match callable.member_realization {
                krusty::libraries::MemberRealization::Dispatch => {
                    // A non-public Kotlin inline member has no legal call instruction: its classfile
                    // method is private and exists only as an inline-body container. Preserve the
                    // dispatch receiver, but route the selected physical handle through the JVM
                    // bytecode splicer. `Callee::Static` is the common IR's existing opaque inline
                    // handle; with a dispatch receiver the emitter prepends `this` to the splice-local
                    // descriptor and emits no static invocation on success. Public inline members keep
                    // ordinary virtual dispatch as their legal fallback.
                    if let krusty::ir::IrPropertyDispatch::Super { owner, interface } =
                        property_dispatch
                    {
                        // A legacy interface body lives only on its receiver-first holder static,
                        // which takes the retained dispatch receiver at its own class.
                        *callee = match callable.nonvirtual_realization.as_deref() {
                            Some(holder) => Box::new(Callee::Static {
                                owner: holder.owner,
                                name: physical_name.clone(),
                                descriptor: holder.descriptor.clone(),
                                inline: krusty::libraries::InlineKind::None,
                            }),
                            None => Box::new(Callee::Special {
                                owner,
                                name: physical_name.clone(),
                                descriptor,
                                interface,
                                source_member: None,
                                source: None,
                            }),
                        };
                    } else if callable.inline.must_inline() {
                        **callee = Callee::Static {
                            owner: callable.physical_owner,
                            name: physical_name.clone(),
                            descriptor,
                            inline: callable.inline,
                        };
                    } else if let Some((owner, descriptor)) = (semantic_role
                        == Some(krusty::types::SemanticCallRole::KotlinAnyHashCode))
                    .then(|| ir.ext_call_source_receiver.get(&expression).copied())
                    .flatten()
                    .and_then(primitive_hash_code)
                    {
                        args.insert(0, dispatch_receiver.take().ok_or(target)?);
                        **callee = Callee::Static {
                            owner,
                            name: physical_name.clone(),
                            descriptor,
                            inline: krusty::libraries::InlineKind::None,
                        };
                    } else if let Some(array) = krusty::jvm::names::array_class_descriptor(
                        callable.physical_owner,
                    )
                    .filter(|_| {
                        !crate::member_dispatch::is_value_class(
                            &dispatch_classifiers,
                            callable.physical_owner,
                        )
                    }) {
                        // An array classifier has no JVM class to dispatch on. Its remaining
                        // dispatched member (`iterator()`) is implemented by the unique static
                        // helper its metadata declares for the array's JVM class. The provider
                        // attaches typed intrinsic realizations to `get`/`set`/`size`, so another
                        // selected array dispatch without this helper is invalid backend input.
                        // A value class over an array (an unsigned array) is a class of its own.
                        let helper = array_member_helper(
                            classpath,
                            &physical_name,
                            &array,
                            &semantic_params,
                            semantic_ret,
                        )
                        .ok_or(target)?;
                        args.insert(0, dispatch_receiver.take().ok_or(target)?);
                        **callee = Callee::Static {
                            owner: krusty::types::type_name(&helper.owner),
                            name: helper.name,
                            descriptor: helper.descriptor,
                            inline: callable.inline,
                        };
                    } else {
                        let (owner, interface) = crate::member_dispatch::virtual_owner(
                            &dispatch_classifiers,
                            callable.physical_owner,
                            callable.owner_is_interface,
                            crate::member_dispatch::VirtualReceiver {
                                dispatch: ir.dispatch_classes.get(&expression).copied(),
                                value_type: ir.ext_call_source_receiver.get(&expression).copied(),
                                annotation_construction: annotation_receiver,
                                semantic_role,
                                declaration_owner: callable.declaration_owner,
                            },
                        )
                        .map_err(|missing| {
                            ExternalDependencyTarget::DispatchClassifier {
                                target,
                                classifier: missing.0,
                            }
                        })?;
                        **callee = Callee::Virtual {
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
                krusty::libraries::MemberRealization::Direct { pass_receiver } => {
                    if krusty::jvm::jvm_libraries::unsigned_intrinsics::realizes_jdk_unsigned_operation(
                        &callable,
                    ) {
                        default_call_operands.record_intrinsic_call(expression);
                    }
                    if pass_receiver {
                        args.insert(0, dispatch_receiver.take().ok_or(target)?);
                    } else {
                        *dispatch_receiver = None;
                    }
                    **callee = Callee::Static {
                        owner: callable.physical_owner,
                        name: physical_name.clone(),
                        descriptor,
                        inline: callable.inline,
                    };
                }
                krusty::libraries::MemberRealization::Intrinsic(
                    intrinsic @ (krusty::libraries::CompilerIntrinsic::StringGet
                    | krusty::libraries::CompilerIntrinsic::NullableAnyToString),
                ) => {
                    physical_result = semantic_ret;
                    **callee = Callee::Intrinsic {
                        operation: if intrinsic == krusty::libraries::CompilerIntrinsic::StringGet {
                            krusty::ir::IrIntrinsic::StringGet
                        } else {
                            krusty::ir::IrIntrinsic::NullableAnyToString
                        },
                        ret: semantic_ret,
                    };
                }
                // An array classifier's own members: the provider attached the operation to the
                // exact `get`/`set`/`size` declaration, so the call becomes that array operation
                // (a `size` getter is called from a property-reference adapter body).
                krusty::libraries::MemberRealization::Intrinsic(
                    intrinsic @ (krusty::libraries::CompilerIntrinsic::ArrayGet
                    | krusty::libraries::CompilerIntrinsic::ArraySet
                    | krusty::libraries::CompilerIntrinsic::ArraySize),
                ) => {
                    physical_result = semantic_ret;
                    **callee = Callee::Intrinsic {
                        operation: match intrinsic {
                            krusty::libraries::CompilerIntrinsic::ArrayGet => {
                                krusty::ir::IrIntrinsic::ArrayGet
                            }
                            krusty::libraries::CompilerIntrinsic::ArraySet => {
                                krusty::ir::IrIntrinsic::ArraySet
                            }
                            _ => krusty::ir::IrIntrinsic::ArraySize,
                        },
                        ret: semantic_ret,
                    };
                }
                krusty::libraries::MemberRealization::Intrinsic(
                    krusty::libraries::CompilerIntrinsic::PrimitiveIteratorNext,
                ) => {
                    let receiver = ir.ext_call_source_receiver.get(&expression).copied();
                    *callee = match primitive_iterator_next(callable.physical_owner, receiver) {
                        Some((name, element)) => {
                            physical_result = element;
                            Box::new(Callee::Virtual {
                                owner: callable.physical_owner,
                                name,
                                descriptor: krusty::jvm::names::method_descriptor(&[], element),
                                params: None,
                                interface: false,
                                module_target: None,
                                target: Some(selected),
                            })
                        }
                        None => {
                            let (owner, interface) = crate::member_dispatch::virtual_owner(
                                &dispatch_classifiers,
                                callable.physical_owner,
                                callable.owner_is_interface,
                                crate::member_dispatch::VirtualReceiver {
                                    dispatch: ir.dispatch_classes.get(&expression).copied(),
                                    value_type: ir
                                        .ext_call_source_receiver
                                        .get(&expression)
                                        .copied(),
                                    annotation_construction: annotation_receiver,
                                    semantic_role,
                                    declaration_owner: callable.declaration_owner,
                                },
                            )
                            .map_err(|missing| ExternalDependencyTarget::DispatchClassifier {
                                target,
                                classifier: missing.0,
                            })?;
                            Box::new(Callee::Virtual {
                                owner,
                                name: physical_name.clone(),
                                descriptor,
                                params: None,
                                interface,
                                module_target: None,
                                target: Some(selected),
                            })
                        }
                    };
                }
                krusty::libraries::MemberRealization::Intrinsic(
                    krusty::libraries::CompilerIntrinsic::PrimitiveHashCode,
                ) => {
                    let receiver = ir.ext_call_source_receiver.get(&expression).copied();
                    *callee = match receiver.and_then(primitive_hash_code) {
                        Some((owner, descriptor)) => {
                            args.insert(0, dispatch_receiver.take().ok_or(target)?);
                            Box::new(Callee::Static {
                                owner,
                                name: physical_name.clone(),
                                descriptor,
                                inline: krusty::libraries::InlineKind::None,
                            })
                        }
                        None => {
                            let (owner, interface) = crate::member_dispatch::virtual_owner(
                                &dispatch_classifiers,
                                callable.physical_owner,
                                callable.owner_is_interface,
                                crate::member_dispatch::VirtualReceiver {
                                    dispatch: ir.dispatch_classes.get(&expression).copied(),
                                    value_type: ir
                                        .ext_call_source_receiver
                                        .get(&expression)
                                        .copied(),
                                    annotation_construction: annotation_receiver,
                                    semantic_role,
                                    declaration_owner: callable.declaration_owner,
                                },
                            )
                            .map_err(|missing| ExternalDependencyTarget::DispatchClassifier {
                                target,
                                classifier: missing.0,
                            })?;
                            Box::new(Callee::Virtual {
                                owner,
                                name: physical_name.clone(),
                                descriptor,
                                params: None,
                                interface,
                                module_target: None,
                                target: Some(selected),
                            })
                        }
                    };
                }
                // kotlinc's `ExplicitEquals`: both operands as objects, through `Object.equals`.
                krusty::libraries::MemberRealization::Intrinsic(
                    krusty::libraries::CompilerIntrinsic::PrimitiveEquals,
                ) => {
                    **callee = Callee::Virtual {
                        owner: krusty::types::wk::java_object(),
                        name: physical_name.clone(),
                        descriptor: "(Ljava/lang/Object;)Z".to_string(),
                        params: None,
                        interface: false,
                        module_target: None,
                        target: Some(selected),
                    };
                }
                krusty::libraries::MemberRealization::Intrinsic(_)
                | krusty::libraries::MemberRealization::RangeConstruction { .. } => {
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
                    krusty::libraries::MemberRealization::Direct {
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
            index as krusty::ir::ExprId,
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
        record_protected_dependency_call(ir, realized_call, &callable);
        if let Some(role) = semantic_role {
            ir.semantic_call_roles.insert(realized_call, role);
        }
    }
    // A dependency call this target realized as a builtin operation is that operation's source
    // call, so it states its line as kotlinc's intrinsic of the operation does.
    for expression in realized_calls {
        let IrExpr::Call { callee, .. } = &ir.exprs[expression as usize] else {
            continue;
        };
        let Callee::Intrinsic { ret, .. } = **callee else {
            continue;
        };
        if ir.source_line(expression).is_some() {
            ir.builtin_calls
                .entry(expression)
                .or_insert_with(|| krusty::ir::IrBuiltinCall {
                    kind: krusty::ir::IrBuiltinCallKind::Intrinsic,
                    result: ret,
                    operands: Vec::new(),
                });
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
/// The physical parameters of a call that passes `appended` after a runtime declaration's
/// `source` parameters, once the compiled method's `descriptor` is shown to take exactly those
/// last.
fn appended_physical_parameters(
    source: &[krusty::types::Ty],
    appended: &[krusty::types::Ty],
    descriptor: &str,
) -> Result<Vec<krusty::types::Ty>, String> {
    let (compiled, _) = krusty::jvm::names::parse_method_descriptor(descriptor)
        .ok_or_else(|| format!("has a malformed descriptor {descriptor}"))?;
    let tail = compiled
        .len()
        .checked_sub(appended.len())
        .filter(|&start| start == source.len())
        .map(|start| &compiled[start..])
        .ok_or_else(|| {
            format!(
                "compiles to {descriptor}, not its {} declared and {} appended parameters",
                source.len(),
                appended.len()
            )
        })?;
    for (&ty, &spelled) in appended.iter().zip(tail) {
        let expected = krusty::jvm::names::type_descriptor(ty);
        if expected != spelled {
            return Err(format!(
                "compiles an appended parameter as {spelled}, not {expected}"
            ));
        }
    }
    Ok(source.iter().chain(appended).copied().collect())
}

fn primitive_hash_code(receiver: krusty::types::Ty) -> Option<(krusty::types::TypeName, String)> {
    let scalar = receiver.scalar_value_repr()?;
    if receiver.is_nullable() || scalar != receiver || receiver.is_unsigned() {
        return None;
    }
    let wrapper = krusty::jvm::jvm_class_map::wrapper_type_name(receiver)?;
    let descriptor = krusty::jvm::names::method_descriptor(&[scalar], krusty::types::Ty::Int);
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
    iterator: krusty::types::TypeName,
    receiver: Option<krusty::types::Ty>,
) -> Option<(String, krusty::types::Ty)> {
    use krusty::types::Ty;
    let element = krusty::types::wk::primitive_iterator_element(iterator)?;
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
    parameters: &[krusty::types::Ty],
    result: krusty::types::Ty,
) -> Option<krusty::jvm::inline::StaticMemberRealization> {
    let erased = |ty: krusty::types::Ty| {
        ty.obj_internal()
            .filter(|_| ty.is_array())
            .and_then(krusty::jvm::names::array_class_descriptor)
            .unwrap_or_else(|| krusty::jvm::names::type_descriptor(ty))
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
    callables: &krusty::backend::CheckedBackendCallables,
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
        krusty::jvm::names::method_descriptor(&callable.physical_params, krusty::types::Ty::Unit)
    } else {
        callable.descriptor.clone()
    })
}

/// Translate checked provider-parameter ordinals into the metadata names consumed by the JVM
/// bytecode inliner. Every argument specializes regenerated generic classes; the reified subset
/// additionally specializes runtime marker operations.
fn publish_inline_substitutions(
    ir: &mut IrFile,
    expression: krusty::ir::ExprId,
    target: ExternalCallableId,
    callable: &krusty::backend::BackendCallableFact,
    substitutions: &[krusty::ir::IrCheckedSubstitution],
) {
    if !callable.inline.can_inline() {
        return;
    }
    let Some(signature) = callable.generic_sig.as_deref() else {
        return;
    };
    let bindings = substitutions
        .iter()
        .filter_map(|substitution| {
            let krusty::fir::FirTypeParameterRef::External {
                callable: declaration,
                ordinal,
            } = substitution.parameter
            else {
                return None;
            };
            if declaration != target {
                return None;
            }
            signature.formals.get(ordinal as usize).map(|name| {
                (
                    name.to_string(),
                    substitution.value,
                    substitution.reified_runtime,
                    substitution.reified,
                )
            })
        })
        .collect::<Vec<_>>();
    if !bindings.is_empty() {
        ir.inline_call_type_arguments.insert(
            expression,
            bindings
                .iter()
                .map(|(name, value, _, _)| (name.clone(), *value))
                .collect(),
        );
        let reified = bindings
            .into_iter()
            .filter_map(|(name, _, runtime, reified)| reified.then_some((name, runtime)))
            .collect::<Vec<_>>();
        if !reified.is_empty() {
            ir.reified_call_subst.insert(expression, reified);
        }
    }
}

/// Remember a protected dependency member so a nested caller can reach it through the subclass
/// accessor. The realized expression is the call emission will see; a result wrapper is not.
fn record_protected_dependency_call(
    ir: &mut IrFile,
    expression: krusty::ir::ExprId,
    callable: &krusty::backend::BackendCallableFact,
) {
    if callable.visibility != krusty::types::Visibility::Protected {
        return;
    }
    let IrExpr::Call {
        callee: call_callee,
        ..
    } = ir.expr(expression)
    else {
        return;
    };
    let Callee::Virtual { name, .. } = &**call_callee else {
        return;
    };
    ir.jvm_protected_dependency_calls.insert(
        expression,
        krusty::jvm::protected_dependency_calls::ProtectedDependencyCall {
            owner: callable.physical_owner,
            name: name.clone(),
            parameters: callable.physical_params.clone().into_boxed_slice(),
            parameter_names: crate::parameter_names::dependency_access_bridge_local_variables(
                &callable.parameter_identities,
                &callable.params,
                &callable.physical_params,
                callable.physical_parameter_plan.as_deref(),
                name,
            )
            .into_boxed_slice(),
            result: callable.physical_ret,
        },
    );
}

/// The provider owns physical erasure; checked FIR owns the final semantic result. Preserve both by
/// wrapping the realized physical call at its original expression identity.
pub(super) fn bridge_external_result(
    ir: &mut IrFile,
    index: usize,
    physical: krusty::types::Ty,
    semantic: krusty::types::Ty,
) -> krusty::ir::ExprId {
    if physical == semantic {
        return index as krusty::ir::ExprId;
    }
    let call = ir.exprs[index].clone();
    let call = ir.add_expr(call);
    copy_call_facts(ir, index as krusty::ir::ExprId, call);
    // Realization itself is the authoritative boundary between the checked Kotlin result and the
    // provider's physical result. Do not depend on an incidental pre-existing side-table entry:
    // value-class lowering needs both facts on the cloned operation to distinguish an erased generic
    // read (`Iterator<X>.next(): Object`, which returns a boxed `X`) from a declaration whose value-
    // class result is already returned as its unboxed carrier.
    ir.set_logical_type(call, semantic);
    ir.set_physical_type(call, physical);
    ir.exprs[index] = IrExpr::TypeOp {
        op: krusty::ir::IrTypeOp::ImplicitCoercion,
        arg: call,
        type_operand: semantic,
    };
    ir.jvm_external_result_coercions
        .insert(index as krusty::ir::ExprId);
    call
}

/// Attach the selected declaration's parameter shape to the realized call. The provider shape omits
/// an ordinary dispatch receiver; static member realizations that consume it prepend the receiver type
/// already recorded by checked lowering. Default stubs do the same independently of the member's normal
/// realization. Mask/marker suffixes deliberately have no semantic entries and fall back to their JVM
/// descriptor slots in the representation pass.
fn publish_declared_call_params(
    ir: &mut IrFile,
    expression: krusty::ir::ExprId,
    kind: ExternalCallableKind,
    member_realization: krusty::libraries::MemberRealization,
    default_call: bool,
    declared: Option<Box<[krusty::types::Ty]>>,
) {
    let Some(declared) = declared else {
        return;
    };
    let mut declared = declared.into_vec();
    let consumes_dispatch = kind == ExternalCallableKind::Member
        && (default_call
            || matches!(
                member_realization,
                krusty::libraries::MemberRealization::Direct {
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
    expression: krusty::ir::ExprId,
    target: ExternalCallableId,
    kind: ExternalCallableKind,
    member_realization: krusty::libraries::MemberRealization,
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
                krusty::libraries::MemberRealization::Direct {
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
    use krusty::types::InlineParameterModifier as Modifier;

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
