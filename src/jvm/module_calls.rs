use crate::fir::{CallableId, PropertyId};
use crate::ir::{
    Callee, ExprId, IrCheckedOperation, IrClassifierKind, IrExpr, IrFile, IrFunction,
    IrModuleProperty, IrModuleSource,
};
use crate::jvm::inline::PropertyAccess;
use crate::jvm::property_realizations::PropertyRealizations;
use crate::types::TypeName;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ModuleRealizationTarget {
    Callable(CallableId),
    Property(PropertyId),
    Classifier(TypeName),
    Function(crate::ir::FunId),
}

fn jvm_field_access(property: &IrModuleProperty, stems: &[String]) -> Option<PropertyAccess> {
    if !crate::jvm::property_realizations::jvm_field_eligible(
        &property.annotations,
        property.visibility,
        property.flags,
        property.extension_receiver.is_some(),
        !property.context_parameters.is_empty(),
    ) {
        return None;
    }
    let (owner, is_static) = if let Some(outer) = property.companion_owner {
        (outer, true)
    } else if let Some(owner) = property.owner {
        if owner_is_jvm_interface(property) && !owner_is_singleton(property) {
            return None;
        }
        (owner, owner_is_singleton(property))
    } else {
        (facade_for(property.source, stems)?, true)
    };
    Some(PropertyAccess::Field {
        owner: owner.render(),
        name: property.name.clone(),
        descriptor: crate::jvm::names::type_descriptor(crate::jvm::ir_emit::ir_ty_to_jvm(
            &property.ty,
        )),
        is_static,
    })
}

pub(super) fn owner_is_jvm_interface(property: &IrModuleProperty) -> bool {
    matches!(
        property.owner_kind,
        Some(IrClassifierKind::Interface | IrClassifierKind::Annotation)
    )
}

pub(super) fn property_getter_name(property: &IrModuleProperty) -> String {
    if property.owner_kind == Some(IrClassifierKind::Annotation) {
        property.name.clone()
    } else {
        crate::names::property_getter_name(&property.name)
    }
}

fn owner_is_singleton(property: &IrModuleProperty) -> bool {
    property.owner_kind == Some(IrClassifierKind::Object)
}

fn header_jvm_name(annotations: &[crate::ir::IrHeaderAnnotation]) -> Result<Option<&str>, ()> {
    let Some(annotation) = annotations
        .iter()
        .find(|annotation| annotation.identity.matches("kotlin/jvm/JvmName"))
    else {
        return Ok(None);
    };
    let name = annotation
        .string_arguments
        .first()
        .map(Box::as_ref)
        .filter(|name| !name.is_empty())
        .ok_or(())?;
    Ok(Some(name))
}

fn applied_jvm_name(
    annotations: Option<&crate::ir::DeclarationAnnotations>,
) -> Result<Option<&str>, ()> {
    let Some(annotation) = annotations.and_then(|annotations| {
        annotations
            .applications()
            .find(|annotation| annotation.internal.matches("kotlin/jvm/JvmName"))
    }) else {
        return Ok(None);
    };
    let name = annotation
        .values
        .iter()
        .find_map(|(parameter, value)| {
            (parameter == "name")
                .then_some(value)
                .or_else(|| (annotation.values.len() == 1).then_some(value))
        })
        .and_then(|value| match value {
            crate::ir::AnnoValue::Const(crate::ir::IrConst::String(value)) => value.as_str(),
            _ => None,
        })
        .filter(|name| !name.is_empty())
        .ok_or(())?;
    Ok(Some(name))
}

fn realize_declared_function_names(ir: &mut IrFile) -> Result<(), ModuleRealizationTarget> {
    let mut names = Vec::new();
    for (&function, annotations) in &ir.function_annotations {
        let physical = applied_jvm_name(Some(annotations))
            .map_err(|()| ModuleRealizationTarget::Function(function))?;
        if let Some(physical) = physical {
            let source_name = ir
                .package_functions
                .iter()
                .find(|declaration| declaration.function == function)
                .map(|declaration| declaration.name.clone())
                .or_else(|| {
                    ir.functions
                        .get(function as usize)
                        .map(|function| function.name.clone())
                })
                .ok_or(ModuleRealizationTarget::Function(function))?;
            names.push((function, source_name, physical.to_owned()));
        }
    }
    let mut reference_names = Vec::new();
    for (class, declaration) in ir.classes.iter().enumerate() {
        let Some(reference) = declaration.func_ref.as_ref() else {
            continue;
        };
        let Some(target) = reference.module_target else {
            continue;
        };
        let callable = ir
            .referenced_module_callables
            .get(&target)
            .ok_or(ModuleRealizationTarget::Callable(target))?;
        if let Some(physical) = header_jvm_name(&callable.annotations)
            .map_err(|()| ModuleRealizationTarget::Callable(target))?
        {
            reference_names.push((class, physical.to_owned()));
        }
    }
    for (function, source_name, name) in names {
        ir.fn_source_names.entry(function).or_insert(source_name);
        let declaration = ir
            .functions
            .get_mut(function as usize)
            .ok_or(ModuleRealizationTarget::Function(function))?;
        declaration.name = name;
    }
    for (class, name) in reference_names {
        ir.classes[class]
            .func_ref
            .as_mut()
            .expect("a collected function-reference realization must remain present")
            .call_name = name;
    }
    Ok(())
}

/// Materialize Kotlin/JVM's default bridge operands from one checked semantic call. Common IR
/// carries only supplied arguments plus omitted parameter ordinals; mask words, zero placeholders,
/// and the marker are exclusively JVM ABI.
fn realize_default_arguments(
    ir: &mut IrFile,
    mut parameters: Vec<crate::types::Ty>,
    arguments: Vec<ExprId>,
    defaults: &[u32],
    extension_receiver_parameter: Option<u32>,
) -> Option<(Vec<crate::types::Ty>, Vec<ExprId>)> {
    if defaults.is_empty() {
        return None;
    }
    if defaults.windows(2).any(|pair| pair[0] >= pair[1])
        || defaults
            .iter()
            .any(|parameter| *parameter as usize >= parameters.len())
    {
        return None;
    }
    let logical_count = parameters
        .len()
        .checked_sub(usize::from(extension_receiver_parameter.is_some()))?;
    let mut masks = vec![0i32; logical_count.div_ceil(32).max(1)];
    let mut supplied = arguments.into_iter();
    let mut physical = Vec::with_capacity(parameters.len() + masks.len() + 1);
    for (parameter, ty) in parameters.iter().copied().enumerate() {
        let parameter = u32::try_from(parameter).ok()?;
        if defaults.contains(&parameter) {
            let logical = match extension_receiver_parameter {
                Some(receiver) if parameter > receiver => parameter - 1,
                Some(receiver) if parameter == receiver => return None,
                _ => parameter,
            } as usize;
            masks[logical / 32] |= 1i32 << (logical % 32);
            physical.push(ir.add_expr(IrExpr::Const(crate::ir::IrConst::zero_for_value_type(ty))));
        } else {
            physical.push(supplied.next()?);
        }
    }
    if supplied.next().is_some() {
        return None;
    }
    physical.extend(
        masks
            .iter()
            .map(|mask| ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(*mask)))),
    );
    physical.push(ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null)));
    parameters.extend(std::iter::repeat_n(crate::types::Ty::Int, masks.len()));
    parameters.push(crate::types::Ty::obj("java/lang/Object"));
    Some((parameters, physical))
}

fn local_default_parameters(
    ir: &IrFile,
    function: crate::ir::FunId,
) -> Result<Vec<crate::types::Ty>, ModuleRealizationTarget> {
    let mut parameters = ir
        .functions
        .get(function as usize)
        .ok_or(ModuleRealizationTarget::Function(function))?
        .params
        .clone();
    if let Some(boxed) = ir.default_stub_boxed_params.get(&function) {
        for &(parameter, ty) in boxed {
            *parameters
                .get_mut(parameter)
                .ok_or(ModuleRealizationTarget::Function(function))? = ty;
        }
    }
    Ok(parameters)
}

fn realize_local_default_arguments(
    ir: &mut IrFile,
    function: crate::ir::FunId,
    defaults: &[u32],
    args: Vec<ExprId>,
) -> Result<Vec<ExprId>, ModuleRealizationTarget> {
    let parameters = local_default_parameters(ir, function)?;
    let extension_receiver_parameter = ir.extension_receiver_fns.contains(&function).then(|| {
        u32::try_from(
            ir.fn_context_counts
                .get(&function)
                .copied()
                .unwrap_or_default(),
        )
        .expect("too many context parameters")
    });
    realize_default_arguments(ir, parameters, args, defaults, extension_receiver_parameter)
        .map(|(_, args)| args)
        .ok_or(ModuleRealizationTarget::Function(function))
}

/// Materialize checked default omissions after representation lowering has finalized every JVM
/// parameter carrier. This is deliberately separate from [`realize`]: stable module containers and
/// declaration names can be selected before value-class lowering, while masks, zero placeholders,
/// and marker operands must use the resulting physical parameter types.
pub(super) fn realize_default_calls(
    ir: &mut IrFile,
    stems: &[String],
) -> Result<(), ModuleRealizationTarget> {
    for raw in 0..ir.exprs.len() {
        let replacement = match ir.exprs[raw].clone() {
            IrExpr::Call {
                callee: Callee::LocalWithDefaults { function, defaults },
                dispatch_receiver,
                args,
            } => {
                let args = realize_local_default_arguments(ir, function, &defaults, args)?;
                Some(IrExpr::Call {
                    callee: Callee::LocalDefault(function),
                    dispatch_receiver,
                    args,
                })
            }
            IrExpr::Call {
                callee:
                    Callee::ClassStaticWithDefaults {
                        owner,
                        function,
                        defaults,
                    },
                dispatch_receiver,
                args,
            } => {
                let args = realize_local_default_arguments(ir, function, &defaults, args)?;
                Some(IrExpr::Call {
                    callee: Callee::ClassStaticDefault { owner, function },
                    dispatch_receiver,
                    args,
                })
            }
            IrExpr::Call {
                callee:
                    Callee::ModuleWithDefaults {
                        target,
                        name,
                        params,
                        ret,
                        defaults,
                        dispatch_receiver_ty,
                        extension_receiver_parameter,
                    },
                dispatch_receiver,
                args,
            } => {
                let failure = ModuleRealizationTarget::Callable(target);
                let callable = ir
                    .referenced_module_callables
                    .get(&target)
                    .cloned()
                    .ok_or(failure)?;
                if callable.flags.has(crate::fir::DeclarationFlags::INLINE) {
                    ir.module_inline_calls.insert(raw as ExprId);
                }
                let physical_name = header_jvm_name(&callable.annotations)
                    .map_err(|()| failure)?
                    .unwrap_or(&name)
                    .to_owned();
                let owner = callable
                    .owner
                    .or_else(|| facade_for(callable.source, stems))
                    .ok_or(failure)?;
                let (mut params, mut args) = realize_default_arguments(
                    ir,
                    params,
                    args,
                    &defaults,
                    extension_receiver_parameter,
                )
                .ok_or(failure)?;
                if let Some(receiver) = dispatch_receiver {
                    params.insert(0, dispatch_receiver_ty.ok_or(failure)?);
                    args.insert(0, receiver);
                } else if dispatch_receiver_ty.is_some() {
                    return Err(failure);
                }
                Some(IrExpr::Call {
                    callee: Callee::CrossFile {
                        facade: owner,
                        name: format!("{physical_name}$default"),
                        params,
                        ret,
                        module_target: Some(target),
                        module_default_call: true,
                    },
                    dispatch_receiver: None,
                    args,
                })
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            ir.exprs[raw] = replacement;
        }
    }
    Ok(())
}

/// Map backend-neutral stable module call targets to JVM file facades. Candidate selection and
/// argument mapping are already complete; this pass performs only the JVM container/name
/// realization and therefore never reads imports, scopes, or overload sets.
pub(super) fn realize(
    ir: &mut IrFile,
    stems: &[String],
    classpath: &crate::jvm::classpath::Classpath,
    property_realizations: &mut PropertyRealizations,
) -> Result<(), ModuleRealizationTarget> {
    realize_declared_function_names(ir)?;
    for raw in 0..ir.exprs.len() {
        // `super` dispatch: the checker fixed the supertype declaration, so only the PHYSICAL
        // descriptor is left to choose, and that is a JVM ABI decision derived from the semantic
        // parameter and physical result types.
        if let IrExpr::Call {
            callee:
                Callee::Super {
                    owner,
                    dispatch_owner,
                    enclosing_dispatch,
                    kind,
                    name,
                    params,
                    ret,
                    interface,
                    realization,
                    descriptor,
                    source,
                    defaults,
                    source_member,
                },
            dispatch_receiver,
            args,
        } = ir.exprs[raw].clone()
        {
            if !defaults.is_empty() {
                return Err(source.map_or(
                    ModuleRealizationTarget::Classifier(owner),
                    ModuleRealizationTarget::Callable,
                ));
            }
            let name = match kind {
                crate::fir::FirSuperCallKind::Function => name,
                crate::fir::FirSuperCallKind::PropertyGetter => {
                    crate::names::property_getter_name(&name)
                }
                crate::fir::FirSuperCallKind::PropertySetter => {
                    crate::names::property_setter_name(&name)
                }
            };
            let descriptor = if descriptor.is_empty() {
                crate::jvm::names::method_descriptor(&params, ret)
            } else {
                descriptor
            };
            if enclosing_dispatch {
                let class = ir
                    .classes
                    .iter()
                    .position(|class| class.fq_name_id() == dispatch_owner)
                    .ok_or(ModuleRealizationTarget::Classifier(dispatch_owner))?;
                let receiver = ir.add_expr(IrExpr::GetValue(0));
                let bridge_arguments = (0..params.len())
                    .map(|parameter| {
                        ir.add_expr(IrExpr::GetValue(
                            u32::try_from(parameter + 1).expect("too many super parameters"),
                        ))
                    })
                    .collect::<Vec<_>>();
                let invocation = match realization {
                    crate::libraries::MemberRealization::Dispatch => ir.add_expr(IrExpr::Call {
                        callee: Callee::Special {
                            owner,
                            name,
                            descriptor,
                            interface,
                            source_member,
                            source,
                        },
                        dispatch_receiver: Some(receiver),
                        args: bridge_arguments,
                    }),
                    crate::libraries::MemberRealization::Direct { pass_receiver } => {
                        let mut operands = bridge_arguments;
                        if pass_receiver {
                            operands.insert(0, receiver);
                        }
                        ir.add_expr(IrExpr::Call {
                            callee: Callee::Static {
                                owner,
                                name,
                                descriptor,
                                inline: crate::libraries::InlineKind::None,
                            },
                            dispatch_receiver: None,
                            args: operands,
                        })
                    }
                    crate::libraries::MemberRealization::Intrinsic(_)
                    | crate::libraries::MemberRealization::RangeConstruction { .. } => {
                        return Err(source.map_or(
                            ModuleRealizationTarget::Classifier(owner),
                            ModuleRealizationTarget::Callable,
                        ));
                    }
                };
                let body = if ret == crate::types::Ty::Unit {
                    let returned = ir.add_expr(IrExpr::Return(None));
                    ir.add_expr(IrExpr::Block {
                        stmts: vec![invocation, returned],
                        value: None,
                    })
                } else {
                    let returned = ir.add_expr(IrExpr::Return(Some(invocation)));
                    ir.add_expr(IrExpr::Block {
                        stmts: vec![returned],
                        value: None,
                    })
                };
                let mut bridge_params = Vec::with_capacity(params.len() + 1);
                bridge_params.push(crate::types::Ty::obj_name(dispatch_owner));
                bridge_params.extend(params);
                let function = ir.add_fun(IrFunction {
                    name: format!("$fir$access$super${raw}"),
                    params: bridge_params,
                    ret,
                    body: Some(body),
                    is_static: true,
                    dispatch_receiver: None,
                    param_checks: Vec::new(),
                });
                ir.classes[class].methods.push(function);
                ir.synthetic_methods.insert(function);
                let mut bridge_call_arguments = Vec::with_capacity(args.len() + 1);
                bridge_call_arguments.push(
                    dispatch_receiver.ok_or(ModuleRealizationTarget::Classifier(dispatch_owner))?,
                );
                bridge_call_arguments.extend(args);
                ir.exprs[raw] = IrExpr::Call {
                    callee: Callee::ClassStatic {
                        owner: dispatch_owner,
                        function,
                    },
                    dispatch_receiver: None,
                    args: bridge_call_arguments,
                };
            } else {
                ir.exprs[raw] = match realization {
                    crate::libraries::MemberRealization::Dispatch => IrExpr::Call {
                        callee: Callee::Special {
                            owner,
                            name,
                            descriptor,
                            interface,
                            source_member,
                            source,
                        },
                        dispatch_receiver,
                        args,
                    },
                    crate::libraries::MemberRealization::Direct { pass_receiver } => {
                        let mut operands = args;
                        if pass_receiver {
                            operands.insert(
                                0,
                                dispatch_receiver
                                    .ok_or(ModuleRealizationTarget::Classifier(dispatch_owner))?,
                            );
                        }
                        IrExpr::Call {
                            callee: Callee::Static {
                                owner,
                                name,
                                descriptor,
                                inline: crate::libraries::InlineKind::None,
                            },
                            dispatch_receiver: None,
                            args: operands,
                        }
                    }
                    crate::libraries::MemberRealization::Intrinsic(_)
                    | crate::libraries::MemberRealization::RangeConstruction { .. } => {
                        return Err(source.map_or(
                            ModuleRealizationTarget::Classifier(owner),
                            ModuleRealizationTarget::Callable,
                        ));
                    }
                };
            }
            continue;
        }
        let replacement = match ir.exprs[raw].clone() {
            IrExpr::Call {
                callee:
                    Callee::Module {
                        target,
                        name,
                        params,
                        ret,
                    },
                dispatch_receiver,
                args,
            } => {
                let failure = ModuleRealizationTarget::Callable(target);
                let callable = ir
                    .referenced_module_callables
                    .get(&target)
                    .cloned()
                    .ok_or(failure)?;
                if callable.flags.has(crate::fir::DeclarationFlags::INLINE) {
                    ir.module_inline_calls.insert(raw as ExprId);
                }
                let physical_name = header_jvm_name(&callable.annotations)
                    .map_err(|()| failure)?
                    .unwrap_or(&name)
                    .to_owned();
                if callable.owner.is_some() {
                    return Err(failure);
                }
                let owner = facade_for(callable.source, stems).ok_or(failure)?;
                Some(IrExpr::Call {
                    callee: Callee::CrossFile {
                        facade: owner,
                        name: physical_name,
                        params,
                        ret,
                        module_target: Some(target),
                        module_default_call: false,
                    },
                    dispatch_receiver,
                    args,
                })
            }
            IrExpr::Checked(IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                substitutions: _,
            }) => {
                let property = ir
                    .referenced_module_properties
                    .get(&target)
                    .ok_or(ModuleRealizationTarget::Property(target))?;
                if let Some(access) = jvm_field_access(property, stems) {
                    property_realizations.record_physical(raw as ExprId, access);
                    Some(IrExpr::PropertyRead {
                        receiver: dispatch_receiver,
                        owner: property.owner.unwrap_or(TypeName::ROOT),
                        name: property.name.clone(),
                        ty: property.ty,
                        interface: owner_is_jvm_interface(property),
                        operation: Some(raw as ExprId),
                    })
                } else {
                    Some(realize_property(
                        property,
                        stems,
                        target,
                        dispatch_receiver,
                        extension_receiver,
                        context_arguments,
                        None,
                    )?)
                }
            }
            IrExpr::Checked(IrCheckedOperation::PropertyWrite {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                value,
                substitutions: _,
            }) => {
                let property = ir
                    .referenced_module_properties
                    .get(&target)
                    .ok_or(ModuleRealizationTarget::Property(target))?;
                if let Some(access) = jvm_field_access(property, stems) {
                    if !property.mutable {
                        return Err(ModuleRealizationTarget::Property(target));
                    }
                    property_realizations.record_physical(raw as ExprId, access);
                    Some(IrExpr::PropertyWrite {
                        receiver: dispatch_receiver,
                        owner: property.owner.unwrap_or(TypeName::ROOT),
                        name: property.name.clone(),
                        value,
                        ty: property.ty,
                        interface: owner_is_jvm_interface(property),
                        operation: Some(raw as ExprId),
                    })
                } else {
                    Some(realize_property(
                        property,
                        stems,
                        target,
                        dispatch_receiver,
                        extension_receiver,
                        context_arguments,
                        Some(value),
                    )?)
                }
            }
            IrExpr::SingletonValue { classifier } => {
                let failure = ModuleRealizationTarget::Classifier(classifier);
                let (owner, field) = if let Some(declaration) =
                    ir.referenced_module_classifiers.get(&classifier).copied()
                {
                    if !declaration.singleton {
                        return Err(failure);
                    }
                    if let Some(owner) = declaration.companion_owner {
                        (owner, classifier.nested_segment_ref().to_owned())
                    } else {
                        (classifier, "INSTANCE".to_string())
                    }
                } else {
                    classpath.singleton_storage(classifier).ok_or(failure)?
                };
                Some(IrExpr::ExternalStaticInstance {
                    owner,
                    ty: classifier,
                    field,
                })
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            ir.exprs[raw] = replacement;
        }
    }
    Ok(())
}

fn realize_property(
    property: &IrModuleProperty,
    stems: &[String],
    target: PropertyId,
    dispatch_receiver: Option<crate::ir::ExprId>,
    extension_receiver: Option<crate::ir::ExprId>,
    context_arguments: Vec<crate::ir::ExprId>,
    value: Option<crate::ir::ExprId>,
) -> Result<IrExpr, ModuleRealizationTarget> {
    let failure = ModuleRealizationTarget::Property(target);
    if context_arguments.len() != property.context_parameters.len() {
        return Err(failure);
    }
    let companion_associated = property.companion_associated;
    let ty = property.ty;
    let mutable = property.mutable;
    if value.is_some() && !mutable {
        return Err(failure);
    }
    let mut parameters = property.context_parameters.clone();
    let mut arguments = context_arguments;
    if let Some(receiver_ty) = (!companion_associated)
        .then_some(property.extension_receiver)
        .flatten()
    {
        parameters.push(receiver_ty);
        arguments.push(extension_receiver.ok_or(failure)?);
    } else if extension_receiver.is_some() {
        return Err(failure);
    }
    if let Some(value) = value {
        parameters.push(ty);
        arguments.push(value);
    }
    let ret = if value.is_some() {
        crate::types::Ty::Unit
    } else {
        ty
    };
    let accessor = if value.is_some() {
        crate::names::property_setter_name(&property.name)
    } else {
        property_getter_name(property)
    };
    if let Some(classifier) = property.owner {
        if property.owner_kind.is_none() {
            return Err(failure);
        }
        Ok(IrExpr::Call {
            callee: Callee::Virtual {
                owner: classifier,
                name: accessor,
                descriptor: String::new(),
                params: Some((parameters, ret)),
                interface: owner_is_jvm_interface(property),
            },
            dispatch_receiver: Some(dispatch_receiver.ok_or(failure)?),
            args: arguments,
        })
    } else {
        if dispatch_receiver.is_some() {
            return Err(failure);
        }
        let facade = facade_for(property.source, stems).ok_or(failure)?;
        Ok(IrExpr::Call {
            callee: Callee::CrossFile {
                facade,
                name: accessor,
                params: parameters,
                ret,
                module_target: None,
                module_default_call: false,
            },
            dispatch_receiver: None,
            args: arguments,
        })
    }
}

pub(super) fn facade_for(
    source: IrModuleSource,
    stems: &[String],
) -> Option<crate::types::TypeName> {
    let stem = stems.get(source.source.raw() as usize)?;
    let package = (source.package != crate::types::TypeName::ROOT).then(|| source.package.render());
    Some(crate::types::type_name(
        &crate::jvm::names::file_class_name(stem, package.as_deref()),
    ))
}
