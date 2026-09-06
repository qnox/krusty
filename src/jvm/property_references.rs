//! JVM realization of checked property-reference declarations.

use super::classpath::{Classpath, ExternalCallableKind, ExternalCallableRealization};
use crate::fir::{
    FirCallableReferenceBinding, FirPropertyReferenceTarget, FirPropertyTarget, PropertyId,
};
use crate::ir::{IrCheckedOperation, IrClass, IrExpr, IrFile, IrModuleProperty, PropRef};
use crate::types::{type_name, Ty, TypeName};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PropertyReferenceRealizationTarget {
    Module(PropertyId),
    External(crate::fir::ExternalCallableId),
    Invalid,
}

pub(super) fn realize(
    ir: &mut IrFile,
    stems: &[String],
    classpath: &Classpath,
    current_facade: &str,
) -> Result<(), PropertyReferenceRealizationTarget> {
    let expression_count = ir.exprs.len();
    for raw in 0..expression_count {
        if let IrExpr::LocalPropertyReference {
            name,
            property_type,
        } = ir.exprs[raw].clone()
        {
            ir.exprs[raw] = local_property_reference(ir, name, property_type);
            continue;
        }
        let IrExpr::Checked(IrCheckedOperation::PropertyReference {
            target,
            delegated,
            binding,
            dispatch_receiver,
            extension_receiver,
            mutable,
            ..
        }) = ir.exprs[raw].clone()
        else {
            continue;
        };
        if dispatch_receiver.is_some() && extension_receiver.is_some() {
            return Err(PropertyReferenceRealizationTarget::Invalid);
        }
        let receiver = dispatch_receiver.or(extension_receiver);
        let delegated_target = if delegated {
            match &target {
                FirPropertyReferenceTarget::Module(target) => Some(*target),
                FirPropertyReferenceTarget::SpecializedModule { .. }
                | FirPropertyReferenceTarget::Classifier { .. }
                | FirPropertyReferenceTarget::External { .. } => {
                    return Err(PropertyReferenceRealizationTarget::Invalid);
                }
            }
        } else {
            None
        };
        let property = match target {
            FirPropertyReferenceTarget::Module(target) => module_property(
                ir.referenced_module_properties
                    .get(&target)
                    .ok_or(PropertyReferenceRealizationTarget::Module(target))?,
                stems,
                target,
                mutable,
            )?,
            FirPropertyReferenceTarget::SpecializedModule { property, .. } => module_property(
                ir.referenced_module_properties
                    .get(&property)
                    .ok_or(PropertyReferenceRealizationTarget::Module(property))?,
                stems,
                property,
                mutable,
            )?,
            FirPropertyReferenceTarget::Classifier {
                owner,
                property,
                property_type,
            } => classifier_property(owner, property, property_type.get()),
            FirPropertyReferenceTarget::External {
                name,
                reflection_owner,
                getter,
                setter,
                extension_receiver,
                property_type,
            } => external_property(
                classpath,
                &name,
                reflection_owner.map(crate::fir::ResolvedTy::get),
                getter.as_ref(),
                setter.as_deref(),
                extension_receiver,
                property_type.get(),
            )?,
        };
        if mutable != property.mutable || mutable != property.setter_name.is_some() {
            return Err(PropertyReferenceRealizationTarget::Invalid);
        }
        let semantic_receiver = property
            .ext_facade
            .as_ref()
            .map(|_| property.owner_internal)
            .or_else(|| (!property.static_dispatch).then_some(property.owner_internal))
            .flatten();
        match binding {
            FirCallableReferenceBinding::Bound
                if receiver.is_none() && semantic_receiver.is_some() =>
            {
                return Err(PropertyReferenceRealizationTarget::Invalid);
            }
            FirCallableReferenceBinding::Unbound
                if receiver.is_some() || semantic_receiver.is_none() =>
            {
                return Err(PropertyReferenceRealizationTarget::Invalid);
            }
            FirCallableReferenceBinding::Static
                if receiver.is_some() || semantic_receiver.is_some() =>
            {
                return Err(PropertyReferenceRealizationTarget::Invalid);
            }
            FirCallableReferenceBinding::Bound
            | FirCallableReferenceBinding::Unbound
            | FirCallableReferenceBinding::Static => {}
        }
        ir.exprs[raw] = if delegated {
            let target = delegated_target.expect("delegated target was validated above");
            let declaration = ir
                .referenced_module_properties
                .get(&target)
                .cloned()
                .ok_or(PropertyReferenceRealizationTarget::Module(target))?;
            synthesize_delegated(ir, stems, target, &declaration, property)?
        } else {
            synthesize(ir, current_facade, property, receiver)
        };
    }
    Ok(())
}

/// Realize the compiler-generated metadata value passed to a delegate convention. Unlike a
/// source-written `::property`, this value needs no generated implementation class: Kotlin's JVM
/// runtime provides concrete `PropertyReferenceNImpl` classes for precisely this declaration-only
/// shape. The selected property, receiver arity, name, and signature all come from checked common
/// IR; this function chooses only their JVM representation.
fn synthesize_delegated(
    ir: &mut IrFile,
    stems: &[String],
    target: PropertyId,
    declaration: &IrModuleProperty,
    property: PropRef,
) -> Result<IrExpr, PropertyReferenceRealizationTarget> {
    let failure = PropertyReferenceRealizationTarget::Module(target);
    if !declaration.context_parameters.is_empty() {
        return Err(failure);
    }
    let receiver_arity = if declaration.companion_associated {
        0
    } else {
        usize::from(declaration.owner.is_some())
            + usize::from(declaration.extension_receiver.is_some())
    };
    if receiver_arity > 2 {
        return Err(failure);
    }
    let reflection_owner = property.owner_internal.ok_or(failure)?;
    let owner = ir.add_expr(IrExpr::ClassConst {
        internal: Some(reflection_owner),
    });
    let name = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
        declaration.name.clone().into(),
    )));
    let mut parameters = declaration.context_parameters.clone();
    if !declaration.companion_associated {
        parameters.extend(declaration.extension_receiver);
    }
    let descriptor = property
        .getter_descriptor
        .clone()
        .unwrap_or_else(|| crate::jvm::names::method_descriptor(&parameters, declaration.ty));
    let signature = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
        format!("{}{descriptor}", property.getter_name).into(),
    )));
    let flags = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(i32::from(
        declaration.owner.is_none(),
    ))));
    let internal = type_name(&format!(
        "kotlin/jvm/internal/PropertyReference{receiver_arity}Impl"
    ));
    // Resolve the facade while the module-source table is still available. A top-level declaration
    // uses it as its reflection owner; member declarations already carry their classifier owner.
    if declaration.owner.is_none()
        && super::module_calls::facade_for(declaration.source, stems).is_none()
    {
        return Err(failure);
    }
    Ok(IrExpr::New {
        internal,
        args: vec![owner, name, signature, flags],
        ctor_params: None,
        ctor_desc: Some("(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V".to_string()),
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    })
}

fn local_property_reference(ir: &mut IrFile, name: Box<str>, property_type: Ty) -> IrExpr {
    let owner = ir.add_expr(IrExpr::ClassConst { internal: None });
    let name_value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
        name.to_string().into(),
    )));
    let getter = crate::names::property_getter_name(&name);
    let descriptor = crate::jvm::names::method_descriptor(&[], property_type);
    let signature = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
        format!("{getter}{descriptor}").into(),
    )));
    let flags = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
    IrExpr::New {
        internal: type_name("kotlin/jvm/internal/PropertyReference0Impl"),
        args: vec![owner, name_value, signature, flags],
        ctor_params: None,
        ctor_desc: Some("(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V".to_string()),
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    }
}

fn classifier_property(
    owner: TypeName,
    property: crate::fir::FirClassifierProperty,
    property_type: Ty,
) -> PropRef {
    let (name, getter) = match property {
        crate::fir::FirClassifierProperty::EnumEntries => ("entries", "getEntries"),
    };
    PropRef {
        owner_internal: Some(owner),
        call_owner_internal: Some(owner),
        prop_name: name.to_string(),
        getter_name: getter.to_string(),
        getter_descriptor: None,
        setter_name: None,
        setter_descriptor: None,
        boxed_value_class: None,
        owner_is_interface: false,
        prop_ty: property_type,
        bound: false,
        static_dispatch: true,
        mutable: false,
        ext_facade: None,
    }
}

fn external_property(
    classpath: &Classpath,
    name: &str,
    reflection_owner: Option<Ty>,
    getter: &FirPropertyTarget,
    setter: Option<&FirPropertyTarget>,
    extension_receiver: bool,
    property_type: Ty,
) -> Result<PropRef, PropertyReferenceRealizationTarget> {
    let getter = external_accessor(classpath, getter, false)?;
    let setter = setter
        .map(|setter| external_accessor(classpath, setter, true))
        .transpose()?;
    let failure = PropertyReferenceRealizationTarget::External(getter.0);
    let field_realization = matches!(
        getter.1.kind,
        ExternalCallableKind::InstanceFieldRead | ExternalCallableKind::StaticFieldRead
    );
    let setter_matches = setter.as_ref().is_none_or(|(_, setter)| {
        if getter.1.kind == ExternalCallableKind::InstanceFieldRead {
            setter.kind == ExternalCallableKind::InstanceFieldWrite
        } else if getter.1.kind == ExternalCallableKind::StaticFieldRead {
            setter.kind == ExternalCallableKind::StaticFieldWrite
        } else {
            setter.kind == getter.1.kind
        }
    });
    if !setter_matches {
        return Err(failure);
    }
    let callable = &getter.1.callable;
    let descriptor = if field_realization {
        crate::jvm::names::method_descriptor(&[], callable.physical_ret)
    } else {
        callable_descriptor(callable)
    };
    let owner = reflection_owner
        .and_then(Ty::kotlin_class_internal)
        .or_else(|| {
            matches!(
                getter.1.kind,
                ExternalCallableKind::TopLevel | ExternalCallableKind::StaticFieldRead
            )
            .then_some(callable.owner)
        })
        .ok_or(failure)?;
    let (static_dispatch, ext_facade) = match getter.1.kind {
        ExternalCallableKind::TopLevel => (true, None),
        ExternalCallableKind::Extension => (false, Some(Some(callable.owner))),
        ExternalCallableKind::Member | ExternalCallableKind::InstanceFieldRead => {
            (false, extension_receiver.then_some(Some(callable.owner)))
        }
        ExternalCallableKind::StaticFieldRead => (true, None),
        ExternalCallableKind::Constructor
        | ExternalCallableKind::InstanceFieldWrite
        | ExternalCallableKind::StaticFieldWrite => {
            return Err(failure);
        }
    };
    Ok(PropRef {
        owner_internal: Some(owner),
        call_owner_internal: Some(callable.owner),
        prop_name: name.to_string(),
        getter_name: callable.name.clone(),
        getter_descriptor: Some(descriptor),
        setter_name: setter
            .as_ref()
            .map(|(_, setter)| setter.callable.name.clone()),
        setter_descriptor: setter.as_ref().map(|(_, setter)| {
            if field_realization {
                crate::jvm::names::method_descriptor(&setter.callable.physical_params, Ty::Unit)
            } else {
                callable_descriptor(&setter.callable)
            }
        }),
        boxed_value_class: None,
        owner_is_interface: callable.owner_is_interface,
        prop_ty: property_type,
        bound: false,
        static_dispatch,
        mutable: setter.is_some(),
        ext_facade,
    })
}

fn external_accessor(
    classpath: &Classpath,
    target: &FirPropertyTarget,
    write: bool,
) -> Result<
    (crate::fir::ExternalCallableId, ExternalCallableRealization),
    PropertyReferenceRealizationTarget,
> {
    let FirPropertyTarget::External { property, .. } = target else {
        return Err(PropertyReferenceRealizationTarget::Invalid);
    };
    let realization = classpath
        .external_property(*property)
        .ok_or(PropertyReferenceRealizationTarget::Invalid)?;
    let accessor = if write {
        realization
            .setter
            .ok_or(PropertyReferenceRealizationTarget::Invalid)?
    } else {
        realization.getter
    };
    classpath
        .external_callable(accessor)
        .map(|realization| (accessor, realization))
        .ok_or(PropertyReferenceRealizationTarget::External(accessor))
}

fn callable_descriptor(callable: &crate::libraries::LibraryCallable) -> String {
    if callable.descriptor.is_empty() {
        crate::jvm::names::method_descriptor(&callable.physical_params, callable.physical_ret)
    } else {
        callable.descriptor.clone()
    }
}

fn module_property(
    property: &IrModuleProperty,
    stems: &[String],
    target: PropertyId,
    reference_mutable: bool,
) -> Result<PropRef, PropertyReferenceRealizationTarget> {
    let failure = PropertyReferenceRealizationTarget::Module(target);
    if !property.context_parameters.is_empty() || reference_mutable && !property.mutable {
        return Err(failure);
    }
    let name = &property.name;
    let declaration_facade =
        super::module_calls::facade_for(property.source, stems).ok_or(failure)?;
    let enclosing = property.owner;
    let companion_associated = property.companion_associated;
    let access_bridge = enclosing.is_some()
        && (property.visibility.is_private() || reference_mutable && property.setter_is_private);
    let owner = property
        .extension_receiver
        .and_then(Ty::kotlin_class_internal)
        .or(enclosing)
        .unwrap_or(declaration_facade);
    // A companion-block receiver names the reflected classifier but is not passed to the accessor.
    // Keep the semantic owner (`C`) separate from the physical file-facade call owner.
    let static_dispatch =
        companion_associated || (property.extension_receiver.is_none() && enclosing.is_none());
    let ext_facade = if access_bridge {
        Some(owner)
    } else if companion_associated {
        None
    } else {
        property.extension_receiver.map(|_| declaration_facade)
    }
    .map(Some);
    let getter_descriptor = if access_bridge {
        Some(crate::jvm::names::method_descriptor(
            &[Ty::obj_name(owner)],
            property.ty,
        ))
    } else {
        property.extension_receiver.map(|receiver| {
            crate::jvm::names::method_descriptor(std::slice::from_ref(&receiver), property.ty)
        })
    };
    let setter_descriptor = if access_bridge && reference_mutable {
        Some(crate::jvm::names::method_descriptor(
            &[Ty::obj_name(owner), property.ty],
            Ty::Unit,
        ))
    } else if reference_mutable {
        property.extension_receiver.map(|receiver| {
            crate::jvm::names::method_descriptor(&[receiver, property.ty], Ty::Unit)
        })
    } else {
        None
    };
    Ok(PropRef {
        owner_internal: Some(owner),
        call_owner_internal: Some(enclosing.unwrap_or(declaration_facade)),
        prop_name: name.to_string(),
        getter_name: if access_bridge {
            format!("access${}$p", crate::names::property_getter_name(name))
        } else {
            super::module_calls::property_getter_name(property)
        },
        getter_descriptor,
        setter_name: reference_mutable.then(|| {
            if access_bridge {
                format!("access${}$p", crate::names::property_setter_name(name))
            } else {
                crate::names::property_setter_name(name)
            }
        }),
        setter_descriptor,
        boxed_value_class: None,
        owner_is_interface: super::module_calls::owner_is_jvm_interface(property),
        prop_ty: property.ty,
        bound: false,
        static_dispatch,
        mutable: reference_mutable,
        ext_facade,
    })
}

fn synthesize(
    ir: &mut IrFile,
    current_facade: &str,
    mut property: PropRef,
    receiver: Option<crate::ir::ExprId>,
) -> IrExpr {
    let bound = receiver.is_some();
    property.bound = bound;
    let arity = usize::from(!bound && !property.static_dispatch);
    let superclass = match (arity, property.mutable) {
        (0, false) => "kotlin/jvm/internal/PropertyReference0Impl",
        (0, true) => "kotlin/jvm/internal/MutablePropertyReference0Impl",
        (1, false) => "kotlin/jvm/internal/PropertyReference1Impl",
        (1, true) => "kotlin/jvm/internal/MutablePropertyReference1Impl",
        _ => unreachable!("property references have at most one receiver"),
    };
    let internal = type_name(&format!(
        "{current_facade}$fir$property${}",
        ir.classes.len()
    ));
    let mut class = IrClass::synthetic(internal);
    class.superclass = type_name(superclass);
    class.prop_ref = Some(property);
    let class = ir.add_class(class);
    match receiver {
        Some(receiver) => IrExpr::New {
            internal,
            args: vec![receiver],
            ctor_params: Some(vec![Ty::obj("kotlin/Any")]),
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        },
        None => IrExpr::StaticInstance {
            owner: class,
            ty: class,
            field: "INSTANCE",
        },
    }
}
