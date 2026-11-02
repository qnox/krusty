use crate::fir::{CallableId, PropertyId};
use crate::ir::{
    Callee, ExprId, IrCheckedOperation, IrClassifierKind, IrExpr, IrFile, IrFunction,
    IrModuleProperty, IrModuleSource, IrStaticPlacement,
};
use crate::jvm::inline::PropertyAccess;
use crate::jvm::property_realizations::PropertyRealizations;
use crate::types::TypeName;

use super::file_facades::FileClasses;

use super::default_call_operands::{DefaultCallOperand, DefaultCallOperands};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ModuleRealizationTarget {
    Callable(CallableId),
    DependencyCallable(crate::fir::ExternalCallableId),
    Property(PropertyId),
    Classifier(TypeName),
    Function(crate::ir::FunId),
    Static(u32),
}

fn jvm_field_access(property: &IrModuleProperty, facades: &[String]) -> Option<PropertyAccess> {
    let (owner, is_static) = jvm_field_storage(property, facades)?;
    Some(PropertyAccess::Field {
        owner,
        name: property.name.clone(),
        descriptor: crate::jvm::names::type_descriptor(crate::jvm::physical_type::ir_ty_to_jvm(
            &property.ty,
        )),
        is_static,
    })
}

/// The class and staticness of the field a module property is realized as, when `@JvmField`
/// makes its storage the declaration's JVM surface.
pub(super) fn jvm_field_storage(
    property: &IrModuleProperty,
    facades: &[String],
) -> Option<(TypeName, bool)> {
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
        (
            static_owner(property.placement, property.source, facades)?,
            true,
        )
    };
    Some((owner, is_static))
}

pub(super) fn owner_is_jvm_interface(property: &IrModuleProperty) -> bool {
    matches!(
        property.owner_kind,
        Some(IrClassifierKind::Interface | IrClassifierKind::Annotation)
    )
}

/// Whether the owner of a static call is a JVM interface, so that the call is named through an
/// interface method reference: a class of this file (a plugin's static `serializer()` on an
/// interface), or the declaring classifier of a module callable (a sibling file's `foo$default`).
pub(super) fn static_owner_is_jvm_interface(
    ir: &IrFile,
    owner: TypeName,
    target: Option<CallableId>,
) -> bool {
    let module_owner_is_interface = target
        .and_then(|target| ir.referenced_module_callables.get(&target))
        .is_some_and(|callable| {
            matches!(
                callable.owner_kind,
                Some(IrClassifierKind::Interface | IrClassifierKind::Annotation)
            )
        });
    module_owner_is_interface
        || ir
            .classes
            .iter()
            .any(|class| class.is_interface && class.fq_name_id() == owner)
}

/// The JVM name of a module property's accessor: the `@JvmName` that lands on it (written on the
/// accessor or targeted with `@get:`/`@set:`), else Kotlin's accessor convention. An annotation
/// member's getter is the member itself.
pub(super) fn module_property_accessor_name(
    property: &IrModuleProperty,
    setter: bool,
) -> Result<String, ()> {
    if let Some(written) = module_property_written_accessor_name(property, setter)? {
        return Ok(written.to_owned());
    }
    Ok(if setter {
        crate::names::property_setter_name(&property.name)
    } else if property.owner_kind == Some(IrClassifierKind::Annotation) {
        property.name.clone()
    } else {
        crate::names::property_getter_name(&property.name)
    })
}

/// The `@JvmName` written for a module property's accessor, if any.
pub(super) fn module_property_written_accessor_name(
    property: &IrModuleProperty,
    setter: bool,
) -> Result<Option<&str>, ()> {
    header_jvm_name(if setter {
        &property.setter_annotations
    } else {
        &property.getter_annotations
    })
}

fn owner_is_singleton(property: &IrModuleProperty) -> bool {
    property.owner_kind == Some(IrClassifierKind::Object)
}

/// The JVM name a `@JvmName` among `annotations` writes: its constant string argument.
pub(super) fn header_jvm_name(
    annotations: &[crate::ir::IrHeaderAnnotation],
) -> Result<Option<&str>, ()> {
    let Some(annotation) = annotations
        .iter()
        .find(|annotation| annotation.identity == crate::types::wk::jvm_name())
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

pub(super) fn applied_jvm_name(
    annotations: Option<&crate::ir::DeclarationAnnotations>,
) -> Result<Option<&str>, ()> {
    let Some(annotation) = annotations.and_then(|annotations| {
        annotations
            .applications()
            .find(|annotation| annotation.internal == crate::types::wk::jvm_name())
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

/// A compiler-default accessor has no function to rename, so its `@JvmName` (written on the
/// accessor or through `@get:`/`@set:`) becomes the accessor's JVM name on the declaration. A
/// source-written accessor is an ordinary function, which [`realize_declared_function_names`]
/// renames.
fn realize_default_accessor_names(ir: &mut IrFile) -> Result<(), ModuleRealizationTarget> {
    for class in &mut ir.classes {
        let failure = ModuleRealizationTarget::Classifier(class.fq_name);
        for property in &mut class.properties {
            let annotations = &property.accessor_annotations;
            let getter = applied_jvm_name(Some(&annotations.getter)).map_err(|()| failure)?;
            let setter = applied_jvm_name(Some(&annotations.setter)).map_err(|()| failure)?;
            if let Some(getter) = getter.filter(|_| property.getter.is_none()) {
                property.getter_jvm_name = Some(getter.to_owned());
            }
            if let Some(setter) = setter.filter(|_| property.setter.is_none()) {
                property.setter_jvm_name = Some(setter.to_owned());
            }
        }
    }
    let mut names = Vec::new();
    for (index, storage) in ir.statics.iter().enumerate() {
        let Some(annotations) = ir.static_accessor_annotations(index as u32) else {
            continue;
        };
        let failure = ModuleRealizationTarget::Static(index as u32);
        let getter = applied_jvm_name(Some(&annotations.getter))
            .map_err(|()| failure)?
            .filter(|_| storage.accessors.getter == crate::ir::IrStaticAccessor::Default);
        let setter = applied_jvm_name(Some(&annotations.setter))
            .map_err(|()| failure)?
            .filter(|_| storage.accessors.setter == crate::ir::IrStaticAccessor::Default);
        if getter.is_some() || setter.is_some() {
            names.push((index, getter.map(str::to_owned), setter.map(str::to_owned)));
        }
    }
    for (index, getter, setter) in names {
        let storage = &mut ir.statics[index];
        if getter.is_some() {
            storage.getter_jvm_name = getter;
        }
        if setter.is_some() {
            storage.setter_jvm_name = setter;
        }
    }
    Ok(())
}

fn realize_declared_function_names(ir: &mut IrFile) -> Result<(), ModuleRealizationTarget> {
    let mut names = Vec::new();
    for (&function, annotations) in &ir.function_annotations {
        let physical = applied_jvm_name(Some(annotations))
            .map_err(|()| ModuleRealizationTarget::Function(function))?;
        if let Some(physical) = physical {
            let source_name = ir
                .fn_source_names
                .get(&function)
                .cloned()
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
        debug_assert_eq!(ir.fn_source_names.get(&function), Some(&source_name));
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
/// and the marker are exclusively JVM ABI. `parameters` are the real method's; `stub_boxed` names
/// each one the `$default` stub takes boxed instead.
pub(super) fn realize_default_arguments(
    ir: &mut IrFile,
    mut parameters: Vec<crate::types::Ty>,
    arguments: Vec<ExprId>,
    defaults: &[u32],
    extension_receiver_parameter: Option<u32>,
    stub_boxed: &[(usize, crate::types::Ty)],
) -> Option<(Vec<crate::types::Ty>, Vec<ExprId>, Vec<DefaultCallOperand>)> {
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
    let mut plan = Vec::with_capacity(parameters.len() + masks.len() + 1);
    let primitive_bounds = parameters
        .iter()
        .copied()
        .map(super::default_parameter_representation::primitive_bounded_type_parameter)
        .collect::<Vec<_>>();
    for (parameter, ty) in parameters.iter().copied().enumerate() {
        let parameter = u32::try_from(parameter).ok()?;
        let logical = match extension_receiver_parameter {
            Some(receiver) if parameter > receiver => Some(parameter - 1),
            Some(receiver) if parameter == receiver => None,
            _ => Some(parameter),
        };
        if defaults.contains(&parameter) {
            let logical = logical?;
            masks[logical as usize / 32] |= 1i32 << (logical % 32);
            // A defaulted non-null type parameter bounded by a JVM primitive is that primitive on
            // the real method and the JDK wrapper on `$default`. The omitted slot is the primitive
            // zero; emission boxes it with `valueOf`, which is what kotlinc writes even though the
            // stub overwrites the slot when the mask bit is set.
            let produced = primitive_bounds[parameter as usize]
                .map(|(primitive, _)| primitive)
                .unwrap_or(ty);
            let placeholder = ir.add_expr(IrExpr::Const(crate::ir::IrConst::zero_for_value_type(
                produced,
            )));
            physical.push(placeholder);
            plan.push(DefaultCallOperand::synthesized_default(
                placeholder,
                logical,
            ));
        } else {
            let argument = supplied.next()?;
            physical.push(argument);
            plan.push(match logical {
                Some(parameter) => DefaultCallOperand::supplied_parameter(argument, parameter),
                None => DefaultCallOperand::supplied(argument),
            });
        }
    }
    for &(parameter, boxed) in stub_boxed {
        *parameters.get_mut(parameter)? = boxed;
    }
    if supplied.next().is_some() {
        return None;
    }
    for mask in &masks {
        let word = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(*mask)));
        physical.push(word);
        plan.push(DefaultCallOperand::synthesized_abi(word));
    }
    let marker = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
    physical.push(marker);
    plan.push(DefaultCallOperand::synthesized_abi(marker));
    parameters.extend(std::iter::repeat_n(crate::types::Ty::Int, masks.len()));
    parameters.push(crate::types::Ty::obj("java/lang/Object"));
    Some((parameters, physical, plan))
}

fn realize_local_default_arguments(
    ir: &mut IrFile,
    function: crate::ir::FunId,
    defaults: &[u32],
    args: Vec<ExprId>,
) -> Result<(Vec<ExprId>, Vec<DefaultCallOperand>), ModuleRealizationTarget> {
    let parameters = ir
        .functions
        .get(function as usize)
        .ok_or(ModuleRealizationTarget::Function(function))?
        .params
        .clone();
    let stub_boxed = ir
        .default_stub_boxed_params
        .get(&function)
        .cloned()
        .unwrap_or_default();
    let extension_receiver_parameter = ir.extension_receiver_fns.contains(&function).then(|| {
        u32::try_from(
            ir.fn_context_counts
                .get(&function)
                .copied()
                .unwrap_or_default(),
        )
        .expect("too many context parameters")
    });
    realize_default_arguments(
        ir,
        parameters,
        args,
        defaults,
        extension_receiver_parameter,
        &stub_boxed,
    )
    .map(|(_, args, plan)| (args, plan))
    .ok_or(ModuleRealizationTarget::Function(function))
}

/// Materialize checked default omissions after representation lowering has finalized every JVM
/// parameter carrier. This is deliberately separate from [`realize`]: stable module containers and
/// declaration names can be selected before value-class lowering, while masks, zero placeholders,
/// and marker operands must use the resulting physical parameter types.
pub(super) fn realize_default_calls(
    ir: &mut IrFile,
    file_classes: &FileClasses,
    default_call_operands: &mut DefaultCallOperands,
) -> Result<(), ModuleRealizationTarget> {
    for raw in 0..ir.exprs.len() {
        let replacement = match ir.exprs[raw].clone() {
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver,
                args,
            } if let Callee::LocalWithDefaults { function, defaults } = &**call_callee => {
                let (args, plan) = realize_local_default_arguments(ir, *function, defaults, args)?;
                Some((
                    IrExpr::Call {
                        callee: Box::new(Callee::LocalDefault(*function)),
                        dispatch_receiver,
                        args,
                    },
                    plan,
                ))
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver,
                args,
            } if let Callee::ClassStaticWithDefaults {
                owner,
                function,
                defaults,
            } = &**call_callee =>
            {
                let (args, plan) = realize_local_default_arguments(ir, *function, defaults, args)?;
                Some((
                    IrExpr::Call {
                        callee: Box::new(Callee::ClassStaticDefault {
                            owner: *owner,
                            function: *function,
                        }),
                        dispatch_receiver,
                        args,
                    },
                    plan,
                ))
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver,
                args,
            } if let Callee::ModuleWithDefaults {
                target,
                default_provider,
                name,
                params,
                ret,
                defaults,
                dispatch_receiver_ty,
                extension_receiver_parameter,
            } = &**call_callee =>
            {
                let failure = ModuleRealizationTarget::Callable(*target);
                let callable = ir
                    .referenced_module_callables
                    .get(target)
                    .cloned()
                    .ok_or(failure)?;
                let crate::fir::ResolvedFunctionOverrideTarget::Module(default_provider) =
                    default_provider
                else {
                    return Err(failure);
                };
                let provider = ir
                    .referenced_module_callables
                    .get(default_provider)
                    .cloned()
                    .ok_or(ModuleRealizationTarget::Callable(*default_provider))?;
                if callable.flags.has(crate::fir::DeclarationFlags::INLINE) {
                    ir.module_inline_calls.insert(raw as ExprId);
                }
                let physical_name = header_jvm_name(&provider.annotations)
                    .map_err(|()| failure)?
                    .unwrap_or(name)
                    .to_owned();
                let owner = match provider.owner {
                    Some(owner) => owner,
                    None => call_owner(provider.placement, provider.source, file_classes)
                        .ok_or(failure)?,
                };
                let mut stub_boxed =
                    super::default_parameter_representation::defaulted_primitive_bounds(
                        params,
                        &provider.default_parameters,
                    );
                stub_boxed.extend(provider.default_parameters.iter().filter_map(|&parameter| {
                    let declared = *provider.parameters.get(parameter)?;
                    super::default_parameter_representation::default_stub_takes_box(ir, declared)
                        .then_some((parameter, declared))
                }));
                let (mut params, mut args, mut plan) = realize_default_arguments(
                    ir,
                    params.to_vec(),
                    args,
                    defaults,
                    *extension_receiver_parameter,
                    &stub_boxed,
                )
                .ok_or(failure)?;
                // A named object's `@JvmStatic` member has a static stub that takes no receiver.
                let static_member =
                    super::ir_emit::jvm_static_members::module_singleton_static_owner(
                        ir,
                        *default_provider,
                    )
                    .is_some();
                // The static stub takes no receiver and initializes the object itself. A receiver
                // that runs code stays on the call, which evaluates and discards it first.
                let mut discarded_receiver = None;
                match dispatch_receiver {
                    Some(receiver) if static_member => {
                        discarded_receiver =
                            (!super::ir_emit::jvm_static_members::receiver_is_inert(ir, receiver))
                                .then_some(receiver);
                    }
                    Some(receiver) => {
                        params.insert(0, dispatch_receiver_ty.ok_or(failure)?);
                        args.insert(0, receiver);
                        plan.insert(0, DefaultCallOperand::supplied(receiver));
                    }
                    None if dispatch_receiver_ty.is_some() => return Err(failure),
                    None => {}
                }
                Some((
                    IrExpr::Call {
                        callee: Box::new(Callee::CrossFile {
                            facade: owner,
                            name: format!("{physical_name}$default"),
                            params,
                            ret: *ret,
                            module_target: Some(*default_provider),
                            module_default_call: true,
                        }),
                        dispatch_receiver: discarded_receiver,
                        args,
                    },
                    plan,
                ))
            }
            _ => None,
        };
        if let Some((replacement, plan)) = replacement {
            ir.exprs[raw] = replacement;
            default_call_operands.record(raw as ExprId, plan);
        }
    }
    Ok(())
}

/// Realize checked semantic `super` dispatch as the JVM's physical non-virtual call. This is a
/// separate pass because compiler plugins add checked declarations after the streaming frontend
/// has completed, while the backend must remain the sole owner of descriptors and invocation
/// opcodes.
pub(super) fn realize_super_calls(
    ir: &mut IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    property_realizations: &mut PropertyRealizations,
    jvm_default: crate::jvm::ir_emit::JvmDefaultMode,
) -> Result<(), ModuleRealizationTarget> {
    for raw in 0..ir.exprs.len() {
        // `super` dispatch: the checker fixed the supertype declaration, so only the PHYSICAL
        // descriptor is left to choose, and that is a JVM ABI decision derived from the semantic
        // parameter and physical result types.
        let IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } = ir.exprs[raw].clone()
        else {
            continue;
        };
        let Callee::Super {
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
            declaration,
            defaults,
            source_member,
        } = *callee
        else {
            continue;
        };
        // The current-module declaration a realized call keeps as its source. A dependency
        // declaration whose ordinary descriptor is not a legal nonvirtual entry (a legacy
        // interface body) names its holder: realize the selected declaration there, as a
        // receiver-first static. Resolution keeps the declaration it selected.
        let (source, external) = match declaration {
            Some(crate::fir::ResolvedFunctionOverrideTarget::Module(callable)) => {
                (Some(callable), None)
            }
            Some(crate::fir::ResolvedFunctionOverrideTarget::External(external)) => {
                (None, Some(external))
            }
            None => (None, None),
        };
        let holder = match external {
            Some(target) => callables
                .callable(target)
                .ok_or(ModuleRealizationTarget::DependencyCallable(target))?
                .nonvirtual_realization
                .clone(),
            None => None,
        };
        // A holder static is public, so `super@Outer` from an inner class calls it directly
        // with the outer receiver; only a real nonvirtual dispatch needs the outer accessor.
        let enclosing_dispatch = enclosing_dispatch && holder.is_none();
        let (owner, descriptor, realization) = match holder {
            Some(holder) => (
                holder.owner,
                holder.descriptor,
                crate::libraries::MemberRealization::Direct {
                    pass_receiver: true,
                },
            ),
            None => (owner, descriptor, realization),
        };
        if !defaults.is_empty() {
            return Err(source.map_or(
                ModuleRealizationTarget::Classifier(owner),
                ModuleRealizationTarget::Callable,
            ));
        }
        // The accessor's own name, as selection resolved it. Property accessors are published
        // accessor-shaped now — `getValue`/`setValue`, the same shape a dependency provider
        // publishes — so rebuilding `get`/`set` from it here named `getGetValue`, which no
        // class declares: `super<Base>@Outer.value` died at its first read with a
        // `NoSuchMethodError`. `kind` remains the semantic fact for targets that need it;
        // nothing about the JVM spelling is derived from it.
        if let crate::ir::IrSuperCallKind::PropertyGetter(property) = kind {
            property_realizations.record_super_getter(raw as ExprId, property);
        }
        // A class that inherits the selected declaration as a specialized default answers the
        // call with its typed forwarder; its erased entry is a bridge back to that forwarder.
        let specialized = match (declaration, interface, &realization) {
            (Some(declaration), false, crate::libraries::MemberRealization::Dispatch) => {
                crate::jvm::inherited_default_realization::specialized_super_entry(
                    ir,
                    callables,
                    owner,
                    declaration,
                    jvm_default,
                )
            }
            _ => None,
        };
        let (name, descriptor, source) = match specialized {
            // A value-class-specialized forwarder is not the selected declaration's own
            // realization: it no longer names that declaration as its source, and its
            // arguments and result are represented for the substituted signature.
            Some(crate::jvm::inherited_default_realization::SpecializedSuperEntry {
                name,
                descriptor,
                value_class_signature: Some((parameters, result)),
            }) => {
                ir.call_declared_params.insert(raw as ExprId, parameters);
                ir.call_declared_ret.insert(raw as ExprId, result);
                (name, descriptor, None)
            }
            Some(specialized) => (specialized.name, specialized.descriptor, source),
            None if descriptor.is_empty() => (
                name,
                crate::jvm::names::method_descriptor(&params, ret),
                source,
            ),
            None => (name, descriptor, source),
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
                    callee: Box::new(Callee::Special {
                        owner,
                        name,
                        descriptor,
                        interface,
                        source_member,
                        source,
                    }),
                    dispatch_receiver: Some(receiver),
                    args: bridge_arguments,
                }),
                // As for a direct `super` call below, the receiver stays the dispatch receiver,
                // so a receiver-first holder static loads it at its own class.
                crate::libraries::MemberRealization::Direct { pass_receiver } => {
                    ir.add_expr(IrExpr::Call {
                        callee: Box::new(Callee::Static {
                            owner,
                            name,
                            descriptor,
                            inline: crate::libraries::InlineKind::None,
                        }),
                        dispatch_receiver: pass_receiver.then_some(receiver),
                        args: bridge_arguments,
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
                callee: Box::new(Callee::ClassStatic {
                    owner: dispatch_owner,
                    function,
                }),
                dispatch_receiver: None,
                args: bridge_call_arguments,
            };
        } else {
            ir.exprs[raw] = match realization {
                crate::libraries::MemberRealization::Dispatch => IrExpr::Call {
                    callee: Box::new(Callee::Special {
                        owner,
                        name,
                        descriptor,
                        interface,
                        source_member,
                        source,
                    }),
                    dispatch_receiver,
                    args,
                },
                // The receiver stays the call's dispatch receiver: a receiver-first holder
                // static loads it at its own class, as the original dispatch did.
                crate::libraries::MemberRealization::Direct { pass_receiver } => IrExpr::Call {
                    callee: Box::new(Callee::Static {
                        owner,
                        name,
                        descriptor,
                        inline: crate::libraries::InlineKind::None,
                    }),
                    dispatch_receiver: if pass_receiver {
                        Some(
                            dispatch_receiver
                                .ok_or(ModuleRealizationTarget::Classifier(dispatch_owner))?,
                        )
                    } else {
                        None
                    },
                    args,
                },
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
    Ok(())
}

/// Map backend-neutral stable module call targets to JVM file facades. Candidate selection and
/// argument mapping are already complete; this pass performs only the JVM container/name
/// realization and therefore never reads imports, scopes, or overload sets.
pub(super) fn realize(
    ir: &mut IrFile,
    file_classes: &FileClasses,
    classifiers: &dyn crate::backend::BackendClassifierSource,
    callables: &crate::backend::CheckedBackendCallables,
    property_realizations: &mut PropertyRealizations,
    jvm_default: crate::jvm::ir_emit::JvmDefaultMode,
) -> Result<(), ModuleRealizationTarget> {
    let facades = &file_classes.containers[..];
    realize_declared_function_names(ir)?;
    realize_default_accessor_names(ir)?;
    realize_super_calls(ir, callables, property_realizations, jvm_default)?;
    prepare_inherited_default_calls(ir)?;
    for raw in 0..ir.exprs.len() {
        // A property accessor call keeps its declaration's parameter vector (contexts, receiver,
        // value), the same checked fact an ordinary call carries for representation boundaries.
        let mut accessor_parameters = None;
        let mut private_accessor_call = None;
        let mut annotation_member_semantic = None::<(PropertyId, crate::types::Ty)>;
        let mut declared_getter_result = None::<crate::types::Ty>;
        let replacement = match ir.exprs[raw].clone() {
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver,
                args,
            } if let Callee::Module {
                target,
                name,
                params,
                ret,
            } = &**call_callee =>
            {
                let failure = ModuleRealizationTarget::Callable(*target);
                let callable = ir
                    .referenced_module_callables
                    .get(target)
                    .cloned()
                    .ok_or(failure)?;
                if callable.flags.has(crate::fir::DeclarationFlags::INLINE) {
                    ir.module_inline_calls.insert(raw as ExprId);
                }
                let physical_name = header_jvm_name(&callable.annotations)
                    .map_err(|()| failure)?
                    .unwrap_or(name)
                    .to_owned();
                if callable.owner.is_some() {
                    return Err(failure);
                }
                let owner =
                    call_owner(callable.placement, callable.source, file_classes).ok_or(failure)?;
                Some(IrExpr::Call {
                    callee: Box::new(Callee::CrossFile {
                        facade: owner,
                        name: physical_name,
                        params: params.to_vec(),
                        ret: *ret,
                        module_target: Some(*target),
                        module_default_call: false,
                    }),
                    dispatch_receiver,
                    args,
                })
            }
            IrExpr::Checked(ref checked_op)
                if let IrCheckedOperation::PropertyRead {
                    target,
                    dispatch_receiver,
                    extension_receiver,
                    context_arguments,
                    substitutions: _,
                } = &**checked_op =>
            {
                let property = ir
                    .referenced_module_properties
                    .get(target)
                    .cloned()
                    .ok_or(ModuleRealizationTarget::Property(*target))?;
                let mut selected_parameters = property.context_parameters.clone();
                if let Some(receiver) = property.extension_receiver {
                    selected_parameters.push(receiver);
                }
                ir.module_member_accesses.insert(
                    raw as ExprId,
                    crate::ir::IrModuleMemberAccess::Property {
                        target: *target,
                        write: false,
                        selected_parameters: selected_parameters.into_boxed_slice(),
                    },
                );
                if let Some(access) = jvm_field_access(&property, facades) {
                    property_realizations.record_physical(raw as ExprId, access);
                    Some(IrExpr::PropertyRead {
                        receiver: *dispatch_receiver,
                        owner: property.owner.unwrap_or(TypeName::ROOT),
                        name: property.name.clone(),
                        ty: property.ty,
                        interface: owner_is_jvm_interface(&property),
                        operation: Some(raw as ExprId),
                    })
                } else {
                    let RealizedAccessor {
                        call,
                        parameters,
                        semantic,
                        private_accessor,
                    } = realize_property(
                        &property,
                        file_classes,
                        *target,
                        *dispatch_receiver,
                        *extension_receiver,
                        context_arguments.clone(),
                        None,
                    )?;
                    accessor_parameters = Some(parameters);
                    private_accessor_call = private_accessor;
                    annotation_member_semantic = semantic.map(|ty| (*target, ty));
                    // The getter returns the property's declared type: a property declared as a
                    // value class hands back its carrier, never a box.
                    declared_getter_result = semantic.is_none().then_some(property.ty);
                    Some(call)
                }
            }
            IrExpr::Checked(ref checked_op)
                if let IrCheckedOperation::PropertyWrite {
                    target,
                    dispatch_receiver,
                    extension_receiver,
                    context_arguments,
                    value,
                    substitutions: _,
                } = &**checked_op =>
            {
                let property = ir
                    .referenced_module_properties
                    .get(target)
                    .cloned()
                    .ok_or(ModuleRealizationTarget::Property(*target))?;
                let mut selected_parameters = property.context_parameters.clone();
                if let Some(receiver) = property.extension_receiver {
                    selected_parameters.push(receiver);
                }
                selected_parameters.push(property.ty);
                ir.module_member_accesses.insert(
                    raw as ExprId,
                    crate::ir::IrModuleMemberAccess::Property {
                        target: *target,
                        write: true,
                        selected_parameters: selected_parameters.into_boxed_slice(),
                    },
                );
                if let Some(access) = jvm_field_access(&property, facades) {
                    if !property.mutable {
                        return Err(ModuleRealizationTarget::Property(*target));
                    }
                    property_realizations.record_physical(raw as ExprId, access);
                    Some(IrExpr::PropertyWrite {
                        receiver: *dispatch_receiver,
                        owner: property.owner.unwrap_or(TypeName::ROOT),
                        name: property.name.clone(),
                        value: *value,
                        ty: property.ty,
                        interface: owner_is_jvm_interface(&property),
                        operation: Some(raw as ExprId),
                    })
                } else {
                    let RealizedAccessor {
                        call,
                        parameters,
                        private_accessor,
                        ..
                    } = realize_property(
                        &property,
                        file_classes,
                        *target,
                        *dispatch_receiver,
                        *extension_receiver,
                        context_arguments.clone(),
                        Some(*value),
                    )?;
                    accessor_parameters = Some(parameters);
                    private_accessor_call = private_accessor;
                    Some(call)
                }
            }
            IrExpr::Call {
                callee: ref call_callee,
                dispatch_receiver,
                args,
            } if let Callee::Virtual {
                owner,
                name,
                descriptor,
                params,
                interface,
                module_target: Some(target),
                target: virtual_target,
            } = &**call_callee =>
            {
                let failure = ModuleRealizationTarget::Callable(*target);
                let callable = ir.referenced_module_callables.get(target).ok_or(failure)?;
                header_jvm_name(&callable.annotations)
                    .map_err(|()| failure)?
                    .filter(|physical| *physical != name)
                    .map(|physical| IrExpr::Call {
                        callee: Box::new(Callee::Virtual {
                            owner: *owner,
                            name: physical.to_owned(),
                            descriptor: descriptor.to_string(),
                            params: params.clone(),
                            interface: *interface,
                            module_target: Some(*target),
                            target: *virtual_target,
                        }),
                        dispatch_receiver,
                        args,
                    })
            }
            IrExpr::SingletonValue { classifier } => {
                let failure = ModuleRealizationTarget::Classifier(classifier);
                let (owner, field) =
                    super::singleton_storage::of(classifiers, classifier).ok_or(failure)?;
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
        if let Some(parameters) = accessor_parameters {
            ir.call_declared_params
                .insert(raw as ExprId, parameters.into_boxed_slice());
        }
        if let Some(accessor) = private_accessor_call {
            ir.private_property_accessor_calls
                .insert(raw as ExprId, accessor);
        }
        if let Some(declared) = declared_getter_result {
            ir.call_declared_ret.insert(raw as ExprId, declared);
        }
        if let Some((target, semantic)) = annotation_member_semantic {
            let IrExpr::Call {
                callee: call_callee,
                ..
            } = &ir.exprs[raw]
            else {
                return Err(ModuleRealizationTarget::Property(target));
            };
            let Callee::Virtual {
                params: Some((_, physical)),
                ..
            } = &**call_callee
            else {
                return Err(ModuleRealizationTarget::Property(target));
            };
            let physical = *physical;
            if physical != semantic {
                let call =
                    super::external_calls::bridge_external_result(ir, raw, physical, semantic);
                ir.call_declared_ret.insert(call, physical);
            }
        }
    }
    Ok(())
}

/// Bind an inherited default call to its exact provider before value-class lowering changes any
/// physical carrier. Module providers retain the backend-neutral default-call node for the later
/// mask pass, but its declaration signature becomes the provider's; an external provider enters
/// the ordinary dependency realization path with the selected module result kept as its semantic
/// result. Neither path performs lookup or overload selection.
fn prepare_inherited_default_calls(ir: &mut IrFile) -> Result<(), ModuleRealizationTarget> {
    let expression_count = ir.exprs.len();
    for raw in 0..expression_count {
        let expression = raw as ExprId;
        let IrExpr::Call {
            callee: call_callee,
            dispatch_receiver,
            args,
        } = ir.exprs[raw].clone()
        else {
            continue;
        };
        let Callee::ModuleWithDefaults {
            target,
            default_provider,
            name,
            params,
            ret,
            defaults,
            dispatch_receiver_ty,
            extension_receiver_parameter,
        } = *call_callee
        else {
            continue;
        };
        match default_provider {
            crate::fir::ResolvedFunctionOverrideTarget::Module(provider) => {
                if provider == target {
                    continue;
                }
                let provider_header = ir
                    .referenced_module_callables
                    .get(&provider)
                    .cloned()
                    .ok_or(ModuleRealizationTarget::Callable(provider))?;
                let provider_params = provider_header.parameters.into_vec();
                let provider_ret = provider_header.result;
                ir.exprs[raw] = IrExpr::Call {
                    callee: Box::new(Callee::ModuleWithDefaults {
                        target,
                        default_provider,
                        name,
                        params: provider_params.clone(),
                        ret: provider_ret,
                        defaults,
                        dispatch_receiver_ty: provider_header.owner.map(crate::types::Ty::obj_name),
                        extension_receiver_parameter,
                    }),
                    dispatch_receiver,
                    args,
                };
                ir.call_declared_params
                    .insert(expression, provider_params.into_boxed_slice());
                let call =
                    super::external_calls::bridge_external_result(ir, raw, provider_ret, ret);
                ir.call_declared_ret.insert(call, provider_ret);
            }
            crate::fir::ResolvedFunctionOverrideTarget::External(provider) => {
                ir.exprs[raw] = IrExpr::Call {
                    callee: Box::new(Callee::External {
                        target: provider,
                        default_provider: Some(provider),
                        params,
                        ret,
                        substitutions: Vec::new(),
                        defaults: defaults.into_vec(),
                        extension_receiver_parameter,
                    }),
                    dispatch_receiver,
                    args,
                };
                if let Some(receiver) = dispatch_receiver_ty {
                    ir.ext_call_source_receiver.insert(expression, receiver);
                }
            }
        }
    }
    Ok(())
}

/// A module property access realized as an accessor call.
struct RealizedAccessor {
    call: IrExpr,
    /// The declared parameters of the call's source arguments, in argument order. A dispatch
    /// receiver is never one of them, even when the physical accessor takes it first.
    parameters: Vec<crate::types::Ty>,
    /// The semantic result of an annotation member read whose JVM result differs from it.
    semantic: Option<crate::types::Ty>,
    /// Set when the call is a private member property's static accessor.
    private_accessor: Option<crate::ir::PrivatePropertyAccessorCall>,
}

fn realize_property(
    property: &IrModuleProperty,
    file_classes: &FileClasses,
    target: PropertyId,
    dispatch_receiver: Option<crate::ir::ExprId>,
    extension_receiver: Option<crate::ir::ExprId>,
    context_arguments: Vec<crate::ir::ExprId>,
    value: Option<crate::ir::ExprId>,
) -> Result<RealizedAccessor, ModuleRealizationTarget> {
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
    let stored = if property.owner_kind == Some(IrClassifierKind::Annotation) {
        crate::jvm::annotation_kclass::annotation_member_jvm_type(ty)
    } else {
        ty
    };
    let ret = if value.is_some() {
        crate::types::Ty::Unit
    } else {
        stored
    };
    let semantic = (value.is_none() && stored != ty).then_some(ty);
    let setter = value.is_some();
    let accessor = module_property_accessor_name(property, setter).map_err(|()| failure)?;
    let private = private_property_accessor(property, setter, &accessor);
    if let Some(classifier) = property.owner {
        if property.owner_kind.is_none() {
            return Err(failure);
        }
        if let Some(private) =
            private.filter(|_| property.owner_kind == Some(IrClassifierKind::Class))
        {
            // The owner's static accessor takes the instance first. The instance stays the call's
            // dispatch receiver, so the declared parameters remain those of the source arguments;
            // only the descriptor carries the receiver slot.
            let mut physical = vec![crate::types::Ty::obj_name(classifier)];
            physical.extend(parameters.iter().copied());
            let private_accessor =
                private_accessor_call(property, setter, &private, accessor, failure)?;
            return Ok(RealizedAccessor {
                call: IrExpr::Call {
                    callee: Box::new(Callee::Static {
                        owner: classifier,
                        name: private.name,
                        descriptor: crate::jvm::method_descriptors::ir_method_desc(&physical, &ret),
                        inline: crate::libraries::InlineKind::None,
                    }),
                    dispatch_receiver: Some(dispatch_receiver.ok_or(failure)?),
                    args: arguments,
                },
                parameters,
                semantic,
                private_accessor: Some(private_accessor),
            });
        }
        Ok(RealizedAccessor {
            call: IrExpr::Call {
                callee: Box::new(Callee::Virtual {
                    owner: classifier,
                    name: accessor,
                    descriptor: String::new(),
                    params: Some((parameters.clone(), ret)),
                    interface: owner_is_jvm_interface(property),
                    module_target: None,
                    target: Some(if value.is_some() {
                        crate::ir::IrVirtualTarget::PropertySetter(
                            crate::fir::ResolvedPropertyOverrideTarget::Module(target),
                        )
                    } else {
                        crate::ir::IrVirtualTarget::PropertyGetter(
                            crate::fir::ResolvedPropertyOverrideTarget::Module(target),
                        )
                    }),
                }),
                dispatch_receiver: Some(dispatch_receiver.ok_or(failure)?),
                args: arguments,
            },
            parameters,
            semantic,
            private_accessor: None,
        })
    } else {
        if dispatch_receiver.is_some() {
            return Err(failure);
        }
        // A private property's synthetic accessor is the part's own; its declared accessors are
        // invoked through the facade.
        let (facade, name, private_accessor) = match private {
            Some(private) => {
                let call = private_accessor_call(property, setter, &private, accessor, failure)?;
                let part = static_owner(
                    property.placement,
                    property.source,
                    &file_classes.containers,
                )
                .ok_or(failure)?;
                (part, private.name, Some(call))
            }
            None => (
                call_owner(property.placement, property.source, file_classes).ok_or(failure)?,
                accessor,
                None,
            ),
        };
        Ok(RealizedAccessor {
            call: IrExpr::Call {
                callee: Box::new(Callee::CrossFile {
                    facade,
                    name,
                    params: parameters.clone(),
                    ret,
                    module_target: None,
                    module_default_call: false,
                }),
                dispatch_receiver: None,
                args: arguments,
            },
            parameters,
            semantic: None,
            private_accessor,
        })
    }
}

/// The synthetic accessor through which code of another class reaches a PRIVATE module property.
/// Only a non-private inline body of the declaring file names one, and its copy runs in the
/// caller's class, as kotlinc's does: a field-backed accessor is `access$get<X>$p`
/// (`access$set<X>$p`); one the source declares, or a delegate supplies, is `access$get<X>`
/// (`access$set<X>`), which calls it. A `const val` is read as its value and has none.
fn private_property_accessor(
    property: &IrModuleProperty,
    setter: bool,
    accessor: &str,
) -> Option<PrivateAccessor> {
    use crate::fir::DeclarationFlags;
    let visibility = if setter {
        property.setter_visibility
    } else {
        property.visibility
    };
    if !visibility.is_private() || property.flags.has(DeclarationFlags::CONST) {
        return None;
    }
    let declared = property.flags.has(DeclarationFlags::DELEGATED)
        || property.flags.has(if setter {
            DeclarationFlags::CUSTOM_SETTER
        } else {
            DeclarationFlags::CUSTOM_GETTER
        });
    Some(PrivateAccessor {
        name: crate::jvm::private_static_access::member_property_accessor_name(accessor, declared),
        calls_declared_accessor: declared,
    })
}

/// The synthetic accessor of a private module property, and whether it calls a declared accessor
/// rather than touching the backing field.
struct PrivateAccessor {
    name: String,
    calls_declared_accessor: bool,
}

/// The facts the value-class pass realizes a private accessor call's JVM name from. A field
/// accessor (`access$get<X>$p`) keeps its plain name, as kotlinc's does; one that calls a declared
/// accessor embeds that accessor's mangled name unless `@JvmName` writes it.
fn private_accessor_call(
    property: &IrModuleProperty,
    setter: bool,
    private: &PrivateAccessor,
    accessor: String,
    failure: ModuleRealizationTarget,
) -> Result<crate::ir::PrivatePropertyAccessorCall, ModuleRealizationTarget> {
    let written = module_property_written_accessor_name(property, setter).map_err(|()| failure)?;
    Ok(crate::ir::PrivatePropertyAccessorCall {
        mangled_accessor: (private.calls_declared_accessor && written.is_none())
            .then_some(accessor),
        result: if setter {
            crate::types::Ty::Unit
        } else {
            property.ty
        },
    })
}

/// Resolve each foreign inline template's declaring source unit to its JVM file facade.
pub(super) fn resolve_foreign_template_facades(ir: &mut IrFile, facades: &[String]) {
    let templates = ir
        .foreign_template_sources()
        .filter_map(|(function, source)| Some((function, facade_for(source, facades)?)))
        .collect::<Vec<_>>();
    ir.record_foreign_template_facades(templates);
}

/// The class realizing a module declaration with no dispatch owner: its file facade, or the
/// declaring class of a `companion { … }` block member.
pub(super) fn static_owner(
    placement: IrStaticPlacement,
    source: IrModuleSource,
    facades: &[String],
) -> Option<crate::types::TypeName> {
    match placement {
        IrStaticPlacement::Package => facade_for(source, facades),
        IrStaticPlacement::CompanionBlock { declaring_class } => Some(declaring_class),
    }
}

/// The class a call to a static declaration of `source` names: a multifile part's members are
/// invoked through the part's public facade, everything else through its declaring class.
pub(super) fn call_owner(
    placement: IrStaticPlacement,
    source: IrModuleSource,
    file_classes: &FileClasses,
) -> Option<crate::types::TypeName> {
    match (
        placement,
        file_classes
            .multifile_facades
            .get(source.source.raw() as usize)?,
    ) {
        (IrStaticPlacement::Package, Some(facade)) => {
            Some(crate::types::type_name_child(source.package, facade))
        }
        _ => static_owner(placement, source, &file_classes.containers),
    }
}

/// The facade class of `source`, from each source's facade class simple name.
pub(super) fn facade_for(
    source: IrModuleSource,
    facades: &[String],
) -> Option<crate::types::TypeName> {
    let segment = facades.get(source.source.raw() as usize)?;
    Some(crate::types::type_name_child(source.package, segment))
}
