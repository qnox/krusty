use super::classpath::{Classpath, ExternalCallableKind};
use crate::fir::{ExternalCallableId, ExternalPropertyId};
use crate::ir::{Callee, IrCheckedOperation, IrExpr, IrFile};

/// Realize already-selected dependency declarations through the provider table shared with the
/// frontend. This is an exact identity lookup, not name resolution or overload selection.
#[derive(Clone, Copy, Debug)]
pub(super) enum ExternalDependencyTarget {
    Callable(ExternalCallableId),
    Property(ExternalPropertyId),
}

impl From<ExternalCallableId> for ExternalDependencyTarget {
    fn from(target: ExternalCallableId) -> Self {
        Self::Callable(target)
    }
}

impl std::fmt::Display for ExternalDependencyTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Callable(target) => write!(formatter, "callable {}", target.raw()),
            Self::Property(target) => write!(formatter, "property {}", target.raw()),
        }
    }
}

pub(super) fn realize(
    ir: &mut IrFile,
    classpath: &Classpath,
) -> Result<(), ExternalDependencyTarget> {
    let expression_count = ir.exprs.len();
    for index in 0..expression_count {
        let expression = u32::try_from(index).expect("too many common IR expressions");
        let defaults = ir.constructor_default_arguments(expression).to_vec();
        if let IrExpr::New {
            external_target: Some(target),
            ..
        } = ir.exprs[index]
        {
            let realization = classpath.external_callable(target).ok_or_else(|| {
                crate::trace_compiler!(
                    "fir",
                    "missing external constructor realization id={target:?} expression={index}"
                );
                target
            })?;
            if realization.kind != ExternalCallableKind::Constructor {
                return Err(target.into());
            }
            let callable = realization.callable;
            if matches!(
                callable.member_realization,
                crate::libraries::MemberRealization::Direct {
                    pass_receiver: false
                }
            ) && callable.name != "<init>"
                && defaults.is_empty()
            {
                let descriptor = if callable.descriptor.is_empty() {
                    crate::jvm::names::method_descriptor(
                        &callable.physical_params,
                        callable.physical_ret,
                    )
                } else {
                    callable.descriptor.clone()
                };
                let IrExpr::New {
                    args,
                    external_target,
                    ..
                } = &mut ir.exprs[index]
                else {
                    unreachable!()
                };
                let args = std::mem::take(args);
                *external_target = None;
                ir.exprs[index] = IrExpr::Call {
                    callee: Callee::Static {
                        owner: callable.owner,
                        name: callable.name,
                        descriptor,
                        inline: callable.inline,
                    },
                    dispatch_receiver: None,
                    args,
                };
                ir.constructor_default_arguments.remove(&expression);
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
                .then(|| callable.constructor_realization.as_deref())
                .flatten();
            if constructor.is_some_and(|constructor| constructor.owner != callable.owner) {
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
            let IrExpr::New {
                args,
                ctor_desc,
                external_target,
                ..
            } = &mut ir.exprs[index]
            else {
                unreachable!()
            };
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
            ir.constructor_default_arguments.remove(&expression);
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
            let realization = classpath
                .external_property(property)
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
                    params: parameters,
                    ret: result,
                    substitutions: Vec::new(),
                    defaults: Vec::new(),
                },
                dispatch_receiver: receiver,
                args: arguments,
            };
            if let Some(source_receiver) = source_receiver {
                ir.ext_call_source_receiver
                    .insert(expression, source_receiver);
            }
        }
        let (target, semantic_params, semantic_ret, substitutions, defaults) =
            match &ir.exprs[index] {
                IrExpr::Call {
                    callee:
                        Callee::External {
                            target,
                            params,
                            ret,
                            substitutions,
                            defaults,
                        },
                    ..
                } => (
                    *target,
                    params.clone(),
                    *ret,
                    substitutions.clone(),
                    defaults.clone(),
                ),
                _ => continue,
            };
        let realization = classpath.external_callable(target).ok_or_else(|| {
            crate::trace_compiler!(
                "fir",
                "missing external call realization id={target:?} expression={index} semantic_result={semantic_ret:?}"
            );
            target
        })?;
        let kind = realization.kind;
        let callable = realization.callable;
        publish_reified_substitutions(ir, expression, target, &callable, &substitutions);
        let declared_params = callable.declared_params.clone();
        let member_realization = callable.member_realization;
        let descriptor = if callable.descriptor.is_empty() {
            crate::jvm::names::method_descriptor(&callable.physical_params, callable.physical_ret)
        } else {
            callable.descriptor.clone()
        };
        // `Boolean.not()` is a selected Kotlin builtin declaration, but has no JVM method. Keep
        // the provider identity through FIR/common lowering, then realize that exact declaration
        // with the common logical operation at this target boundary. This also covers callable-
        // reference adapters, whose bodies contain the same external-call node as an ordinary call.
        if member_realization
            == crate::libraries::MemberRealization::Intrinsic(
                crate::libraries::CompilerIntrinsic::BooleanNot,
            )
        {
            let receiver = match &ir.exprs[index] {
                IrExpr::Call {
                    dispatch_receiver: Some(receiver),
                    args,
                    ..
                } if kind == ExternalCallableKind::Member
                    && defaults.is_empty()
                    && args.is_empty() =>
                {
                    *receiver
                }
                _ => return Err(target.into()),
            };
            let false_value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(false)));
            ir.exprs[index] = IrExpr::PrimitiveBinOp {
                op: crate::ir::IrBinOp::Eq,
                lhs: receiver,
                rhs: false_value,
            };
            continue;
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
                owner: callable.owner,
                name: callable.name,
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
                owner: callable.owner,
                name: callable.name,
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
                        owner: callable.owner,
                        name: callable.name,
                        ty: semantic_ret,
                        interface: false,
                        operation,
                    };
                }
                ExternalCallableKind::InstanceFieldWrite if arguments.len() == 1 => {
                    let property_ty = callable.params.first().copied().ok_or(target)?;
                    ir.exprs[index] = IrExpr::PropertyWrite {
                        receiver: Some(receiver),
                        owner: callable.owner,
                        name: callable.name,
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
                "realize external default target={target:?} kind={kind:?} owner={} name={} descriptor={} omitted={defaults:?} bridge={:?}",
                callable.owner,
                callable.name,
                callable.descriptor,
                callable.default_realization,
            );
            let default = callable.default_realization.as_deref().ok_or(target)?;
            // Checked FIR carries semantic zero values for omitted arguments. Replace them at this
            // exact provider boundary with zeros for the selected bridge's physical parameters. A
            // value class such as `Duration` is a reference semantically but a primitive `long` in
            // this descriptor; leaving the semantic `null` placeholder makes the bridge unbox null
            // before its mask can substitute the default.
            let extension_receiver = matches!(
                kind,
                ExternalCallableKind::Extension | ExternalCallableKind::Member
            )
            .then_some(callable.source_receiver)
            .flatten()
            .map(|_| callable.context_count);
            let replacements = defaults
                .iter()
                .map(|default_parameter| {
                    let source_parameter = *default_parameter as usize;
                    let physical_parameter = source_parameter
                        + usize::from(
                            extension_receiver.is_some_and(|receiver| source_parameter >= receiver),
                        );
                    let ty = *default.real_params.get(physical_parameter).ok_or(target)?;
                    let value = ir.add_expr(IrExpr::Const(
                        crate::ir::IrConst::zero_for_value_type(ty.canonical_semantic()),
                    ));
                    Ok((source_parameter, value))
                })
                .collect::<Result<Vec<_>, ExternalCallableId>>()?;
            {
                let IrExpr::Call {
                    dispatch_receiver,
                    args,
                    ..
                } = &mut ir.exprs[index]
                else {
                    unreachable!()
                };
                for (parameter, value) in replacements {
                    *args.get_mut(parameter).ok_or(target)? = value;
                }
                match realization.kind {
                    ExternalCallableKind::TopLevel => {}
                    ExternalCallableKind::Extension => {
                        let receiver = dispatch_receiver.take().ok_or(target)?;
                        args.insert(callable.context_count.min(args.len()), receiver);
                    }
                    ExternalCallableKind::Member => {
                        args.insert(0, dispatch_receiver.take().ok_or(target)?);
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
            publish_declared_call_params(
                ir,
                index as crate::ir::ExprId,
                kind,
                member_realization,
                true,
                declared_params,
            );
            bridge_external_result(ir, index, callable.physical_ret, semantic_ret);
            continue;
        }
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
                Some(crate::ir::IrIntrinsic::StringPlus)
            }
            Some(crate::libraries::CompilerIntrinsic::NullableAnyToString) => {
                Some(crate::ir::IrIntrinsic::NullableAnyToString)
            }
            Some(
                crate::libraries::CompilerIntrinsic::ArraySize
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
                | crate::libraries::CompilerIntrinsic::EnumValueOf
                | crate::libraries::CompilerIntrinsic::ForEach
                | crate::libraries::CompilerIntrinsic::ForEachIndexed
                | crate::libraries::CompilerIntrinsic::Map
                | crate::libraries::CompilerIntrinsic::FlatMap
                | crate::libraries::CompilerIntrinsic::IsEmpty
                | crate::libraries::CompilerIntrinsic::IsNotEmpty
                | crate::libraries::CompilerIntrinsic::Count
                | crate::libraries::CompilerIntrinsic::TrimIndent
                | crate::libraries::CompilerIntrinsic::TrimMargin
                | crate::libraries::CompilerIntrinsic::NumericConversion
                | crate::libraries::CompilerIntrinsic::PrimitiveCompare
                | crate::libraries::CompilerIntrinsic::PrimitiveBitAnd
                | crate::libraries::CompilerIntrinsic::PrimitiveBitOr
                | crate::libraries::CompilerIntrinsic::PrimitiveBitXor
                | crate::libraries::CompilerIntrinsic::PrimitiveShiftLeft
                | crate::libraries::CompilerIntrinsic::PrimitiveShiftRight
                | crate::libraries::CompilerIntrinsic::PrimitiveUnsignedShiftRight
                | crate::libraries::CompilerIntrinsic::BooleanNot
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
        match kind {
            ExternalCallableKind::TopLevel => {
                *callee = Callee::Static {
                    owner: callable.owner,
                    name: callable.name,
                    descriptor,
                    inline: callable.inline,
                };
            }
            ExternalCallableKind::Extension => {
                let receiver = dispatch_receiver.take().ok_or(target)?;
                args.insert(callable.context_count.min(args.len()), receiver);
                *callee = Callee::Static {
                    owner: callable.owner,
                    name: callable.name,
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
                        *callee = Callee::Special {
                            owner,
                            name: callable.name,
                            descriptor,
                            interface,
                            source_member: None,
                        };
                    } else if callable.inline.must_inline() {
                        *callee = Callee::Static {
                            owner: callable.owner,
                            name: callable.name,
                            descriptor,
                            inline: callable.inline,
                        };
                    } else {
                        let semantic_array_declaration =
                            crate::types::Ty::obj_name(callable.owner).is_array();
                        *callee = Callee::Virtual {
                            owner: callable.owner,
                            name: callable.name,
                            descriptor,
                            // Primitive/reference arrays are Kotlin classifiers but have no JVM
                            // class on which `get`/`set`/`size` can dispatch. Retain the already
                            // checked declaration shape so the emitter can realize that exact
                            // selected member as an array operation. Ordinary classpath calls keep
                            // their provider descriptor as the sole physical source.
                            params: semantic_array_declaration
                                .then_some((semantic_params.clone(), semantic_ret)),
                            interface: callable.owner_is_interface,
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
                        owner: callable.owner,
                        name: callable.name,
                        descriptor,
                        inline: callable.inline,
                    };
                }
                crate::libraries::MemberRealization::Intrinsic(
                    crate::libraries::CompilerIntrinsic::StringPlus,
                ) => {
                    physical_result = semantic_ret;
                    *callee = Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::StringPlus,
                        ret: semantic_ret,
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
        publish_declared_call_params(
            ir,
            index as crate::ir::ExprId,
            kind,
            member_realization,
            false,
            declared_params,
        );
        bridge_external_result(ir, index, physical_result, semantic_ret);
    }
    for (owner, target) in &mut ir.external_super_constructors {
        let uses_defaults = ir
            .super_constructor_default_arguments
            .get(owner)
            .is_some_and(|parameters| !parameters.is_empty());
        target.descriptor = Some(external_constructor_descriptor(
            classpath,
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
            classpath,
            target.declaration,
            !constructor.default_parameters.is_empty(),
        )?);
    }
    Ok(())
}

fn external_constructor_descriptor(
    classpath: &Classpath,
    target: ExternalCallableId,
    uses_defaults: bool,
) -> Result<String, ExternalCallableId> {
    let realization = classpath.external_callable(target).ok_or(target)?;
    if realization.kind != ExternalCallableKind::Constructor {
        return Err(target);
    }
    let callable = realization.callable;
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
        callable.descriptor
    })
}

/// Translate checked provider-parameter ordinals into the metadata names consumed by the JVM
/// bytecode inliner. The values and declaration identity were fixed in FIR; this backend step only
/// exposes the physical formal spelling carried by the selected dependency declaration.
fn publish_reified_substitutions(
    ir: &mut IrFile,
    expression: crate::ir::ExprId,
    target: ExternalCallableId,
    callable: &crate::libraries::LibraryCallable,
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
fn bridge_external_result(
    ir: &mut IrFile,
    index: usize,
    physical: crate::types::Ty,
    semantic: crate::types::Ty,
) {
    if physical == semantic {
        return;
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
}

/// Keep checker-selected call facts attached to the selected call when a backend boundary wraps it.
/// The wrapper retains the source expression identity; the cloned node retains the operation identity
/// consumed by value-class lowering and the bytecode inliner.
fn copy_call_facts(ir: &mut IrFile, source: crate::ir::ExprId, target: crate::ir::ExprId) {
    if let Some(value) = ir.fir_origins.get(&source).copied() {
        ir.fir_origins.insert(target, value);
    }
    if let Some(value) = ir.expr_lines.get(&source).copied() {
        ir.expr_lines.insert(target, value);
    }
    if let Some(value) = ir.expr_source_lines.get(&source).copied() {
        ir.expr_source_lines.insert(target, value);
    }
    if let Some(value) = ir.expr_end_lines.get(&source).copied() {
        ir.expr_end_lines.insert(target, value);
    }
    if let Some(value) = ir.logical_types.get(&source).copied() {
        ir.logical_types.insert(target, value);
    }
    if let Some(value) = ir.physical_types.get(&source).copied() {
        ir.physical_types.insert(target, value);
    }
    if let Some(value) = ir.ext_call_source_receiver.get(&source).copied() {
        ir.ext_call_source_receiver.insert(target, value);
    }
    if let Some(value) = ir.call_declared_ret.get(&source).copied() {
        ir.call_declared_ret.insert(target, value);
    }
    if let Some(value) = ir.call_declared_params.get(&source).cloned() {
        ir.call_declared_params.insert(target, value);
    }
    if let Some(value) = ir.suspend_calls.get(&source).copied() {
        ir.suspend_calls.insert(target, value);
    }
    if let Some(value) = ir.reified_call_subst.get(&source).cloned() {
        ir.reified_call_subst.insert(target, value);
    }
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
