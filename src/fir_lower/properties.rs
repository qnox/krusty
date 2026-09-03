use crate::fir::{
    DeclarationFlags, DeclarationId, DeclarationKind, FirBody, FirCallTarget,
    FirCallableReferenceBinding, FirDelegateCall, FirDelegateDispatchReceiver,
    FirPropertyReferenceTarget, ResolvedModuleIndex, SourceFileId,
};
use std::collections::HashMap;

use crate::ir::{
    Callee, ExprId, FunId, IrCheckedOperation, IrCheckedProperty, IrExpr, IrField, IrFile,
    IrFunction, IrLocalPropertyLayout, IrNodeOrigin, IrProperty, IrStatic, IrfFlags,
};
use crate::types::{Ty, TypeName};

use super::{
    constructors::classifier_type_parameter_ordinal, generics::declaration_type_parameters,
    lower_body_with_context, FirFileLoweringFailure, LocalCallableLoweringContext,
};

fn declaration_source_order(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Result<u32, FirFileLoweringFailure> {
    index
        .source_order(declaration)
        .ok_or(FirFileLoweringFailure::MissingSourceOrder(declaration))
}

fn named_context_parameters(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    types: &[Ty],
) -> Vec<(String, Ty)> {
    let accessor = index.owned_declaration(declaration, DeclarationKind::Accessor, 0);
    types
        .iter()
        .enumerate()
        .map(|(ordinal, ty)| {
            let name = accessor
                .and_then(|accessor| {
                    index.callable_parameter_name(
                        crate::fir::CallableId::from_raw(accessor.raw()),
                        ordinal as u32,
                    )
                })
                .unwrap_or("_")
                .to_owned();
            (name, *ty)
        })
        .collect()
}

pub(super) fn predeclare_properties(
    index: &ResolvedModuleIndex,
    source: SourceFileId,
    inline_payload_declarations: &std::collections::HashSet<DeclarationId>,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(raw as u32);
        let Some(anchor) = index.declaration_anchor(declaration) else {
            continue;
        };
        if (anchor.source != source && !inline_payload_declarations.contains(&declaration))
            || anchor.kind != DeclarationKind::Property
        {
            continue;
        }
        let body_local = index.is_body_local_declaration(declaration);
        let Some(header) = index.declaration_header(declaration) else {
            // Multiplatform actualization preserves the declaration coordinate while excluding the
            // matched `expect` property from the finalized semantic index. Body-local declarations
            // may also still be waiting for their selected Pass-2 group to publish.
            continue;
        };
        let property = index
            .property_for_declaration(declaration)
            .and_then(|property| index.property(property));
        let Some(property) = property else {
            if body_local {
                continue;
            }
            return Err(FirFileLoweringFailure::MissingProperty(declaration));
        };
        if ir.checked_properties.contains_key(&property.id) {
            continue;
        }
        let Some(signature) = index.signature(declaration) else {
            if body_local {
                continue;
            }
            return Err(FirFileLoweringFailure::MissingProperty(declaration));
        };
        // An enum-entry body is a stable semantic ownership boundary. Its properties belong to
        // the entry subclass skeleton predeclared from that exact owner, not to the enclosing enum
        // classifier returned by ordinary classifier ancestry.
        let entry_class = anchor
            .owner
            .and_then(|owner| ir.checked_enum_entry_classes.get(&owner).copied());
        let class = entry_class
            .map(Ok)
            .or_else(|| {
                index.enclosing_classifier(declaration).map(|classifier| {
                    ir.checked_classifier_classes
                        .get(&classifier.declaration)
                        .copied()
                        .ok_or(FirFileLoweringFailure::MissingClassifier(
                            classifier.declaration,
                        ))
                })
            })
            .transpose()?;
        let name = index
            .declaration_name(declaration)
            .ok_or(FirFileLoweringFailure::MissingProperty(declaration))?
            .to_owned();
        assert!(
            ir.checked_properties
                .insert(
                    property.id,
                    IrCheckedProperty {
                        declaration,
                        initialization_order: header.initialization_order,
                        class,
                        name,
                        // A property always stores/returns a value. Source `Unit` therefore uses
                        // the `kotlin.Unit` singleton reference rather than a void method/field
                        // descriptor; function-return `Unit` remains the distinct effect type.
                        ty: crate::types::stored_value_ty(signature.result.get()),
                        storage_ty: None,
                        visibility: header.visibility,
                        flags: header.flags,
                        initializer: None,
                        delegate: None,
                        delegate_plan: None,
                        getter: None,
                        setter: None,
                    },
                )
                .is_none(),
            "a stable property has one common-IR declaration"
        );
    }
    Ok(())
}

/// Finish the declaration-oriented property structures consumed by common lowering and backends.
/// The checked-property table is deliberately source-oriented while bodies are arriving; this step
/// publishes storage and accessor declarations once every body in the file has been consumed.
pub(super) fn finalize_properties(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    let mut properties = ir
        .checked_properties
        .iter()
        .filter_map(|(id, property)| {
            let anchor = index.declaration_anchor(property.declaration)?;
            Some((anchor, *id, property.clone()))
        })
        .map(|(anchor, id, property)| {
            Ok((
                declaration_source_order(index, property.declaration)?,
                anchor,
                id,
                property,
            ))
        })
        .collect::<Result<Vec<_>, FirFileLoweringFailure>>()?;
    properties.sort_by_key(|(source_order, anchor, _, _)| (*source_order, anchor.sibling));

    let mut realizations = HashMap::new();
    let mut initialization: HashMap<crate::ir::ClassId, Vec<(u32, ExprId)>> = HashMap::new();
    for (source_order, anchor, property_id, property) in properties {
        let shape = index
            .property(property_id)
            .ok_or(FirFileLoweringFailure::MissingProperty(
                property.declaration,
            ))?;
        let context_parameters = index
            .signature(property.declaration)
            .and_then(|signature| {
                signature
                    .parameters
                    .get(..shape.context_parameter_count as usize)
            })
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        // As with companion-block functions, the classifier receiver is retained in the stable
        // property shape solely for associated lookup. Checked FIR has already erased it as a value
        // receiver, so common IR must expose a receiverless top-level realization.
        let extension_receiver = (!property.flags.has(crate::fir::DeclarationFlags::COMPANION))
            .then_some(shape.extension_receiver)
            .flatten()
            .map(crate::fir::ResolvedTy::get);
        match property.class {
            Some(class) if extension_receiver.is_some() => materialize_member_extension_property(
                index,
                source_order,
                property_id,
                property,
                class,
                extension_receiver.expect("guarded member extension receiver"),
                context_parameters,
                ir,
                &mut realizations,
                &mut initialization,
            )?,
            Some(class) => materialize_member_property(
                index,
                anchor,
                source_order,
                property_id,
                property,
                class,
                context_parameters,
                ir,
                &mut realizations,
                &mut initialization,
            )?,
            None => materialize_top_level_property(
                index,
                source_order,
                property_id,
                property,
                extension_receiver,
                context_parameters,
                ir,
                &mut realizations,
            )?,
        }
    }
    merge_class_initialization(ir, initialization)?;
    realize_backing_field_operations(index, ir, &realizations)?;
    ir.local_property_layouts.extend(realizations);
    Ok(())
}

fn materialize_member_extension_property(
    index: &ResolvedModuleIndex,
    source_order: u32,
    property_id: crate::fir::PropertyId,
    property: IrCheckedProperty,
    class_id: crate::ir::ClassId,
    receiver: Ty,
    context_parameters: Vec<Ty>,
    ir: &mut IrFile,
    realizations: &mut HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
    initialization: &mut HashMap<crate::ir::ClassId, Vec<(u32, ExprId)>>,
) -> Result<(), FirFileLoweringFailure> {
    if property.delegate.is_some() || property.delegate_plan.is_some() {
        return materialize_member_extension_delegate(
            index,
            source_order,
            property_id,
            property,
            class_id,
            receiver,
            context_parameters,
            ir,
            realizations,
            initialization,
        );
    }
    if property.initializer.is_some() {
        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            property.declaration,
        ));
    }
    let owner = ir.classes[class_id as usize].fq_name;
    let interface = ir.classes[class_id as usize].is_interface;
    let is_abstract =
        property.getter.is_none() && (interface || property.flags.has(DeclarationFlags::ABSTRACT));
    let mut getter_parameters = context_parameters.clone();
    getter_parameters.push(receiver);
    let getter = match property.getter {
        Some(body) => add_accessor_function(
            ir,
            crate::names::property_getter_name(&property.name),
            getter_parameters,
            property.ty,
            body,
            true,
            Some(owner),
        ),
        None if is_abstract => add_abstract_accessor_function(
            ir,
            crate::names::property_getter_name(&property.name),
            getter_parameters,
            property.ty,
            owner,
        ),
        None => {
            return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ));
        }
    };
    let mutable = property.flags.has(DeclarationFlags::MUTABLE);
    let setter_parameters = || {
        let mut parameters = context_parameters.clone();
        parameters.push(receiver);
        parameters.push(property.ty);
        parameters
    };
    let setter = match property.setter {
        Some(body) => Some(add_accessor_function(
            ir,
            crate::names::property_setter_name(&property.name),
            setter_parameters(),
            Ty::Unit,
            body,
            false,
            Some(owner),
        )),
        None if is_abstract && mutable => Some(add_abstract_accessor_function(
            ir,
            crate::names::property_setter_name(&property.name),
            setter_parameters(),
            Ty::Unit,
            owner,
        )),
        None => None,
    };
    let type_params = declaration_type_parameters(index, property.declaration);
    for function in std::iter::once(getter).chain(setter) {
        ir.fn_source_order.insert(function, source_order);
        ir.fresh_method_decls.push(function);
        if property.visibility.is_private() {
            ir.private_methods.insert(function);
        }
        if property.flags.has(DeclarationFlags::OPEN)
            || property.flags.has(DeclarationFlags::ABSTRACT)
        {
            ir.open_methods.insert(function);
        }
        ir.classes[class_id as usize].methods.push(function);
        if !type_params.is_empty() {
            let signature = &ir.functions[function as usize];
            ir.signatures.insert(
                function,
                crate::ir::IrGenericSig {
                    type_params: type_params.clone(),
                    params: signature.params.clone(),
                    ret: Some(signature.ret),
                    supers: Vec::new(),
                },
            );
        }
    }
    ir.member_ext_props
        .entry(owner)
        .or_default()
        .push(crate::ir::MemberExtProp {
            name: property.name.clone(),
            receiver,
            ty: property.ty,
            is_var: mutable,
            is_abstract,
            getter,
            setter,
            visibility: property.visibility,
            type_params,
        });
    realizations.insert(
        property_id,
        IrLocalPropertyLayout::MemberExtension {
            owner,
            interface,
            name: property.name,
            setter,
            receiver,
            ty: property.ty,
            context_parameters,
        },
    );
    Ok(())
}

fn materialize_top_level_property(
    index: &ResolvedModuleIndex,
    source_order: u32,
    property_id: crate::fir::PropertyId,
    property: IrCheckedProperty,
    extension_receiver: Option<Ty>,
    context_parameters: Vec<Ty>,
    ir: &mut IrFile,
    realizations: &mut HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
) -> Result<(), FirFileLoweringFailure> {
    if property.delegate.is_some() || property.delegate_plan.is_some() {
        return materialize_top_level_delegate(
            source_order,
            property_id,
            property,
            extension_receiver,
            context_parameters,
            index,
            ir,
            realizations,
        );
    }
    let has_storage = property.initializer.is_some()
        || property.flags.has(DeclarationFlags::LATEINIT)
        || property.flags.has(DeclarationFlags::EXPLICIT_BACKING_FIELD);
    if has_storage && property.delegate.is_none() {
        if !context_parameters.is_empty() {
            return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ));
        }
        let init = match property.initializer {
            Some(initializer) => initializer,
            None if property.flags.has(DeclarationFlags::LATEINIT) => {
                ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null))
            }
            None => {
                return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
                    property.declaration,
                ));
            }
        };
        let index = u32::try_from(ir.statics.len())
            .map_err(|_| FirFileLoweringFailure::UnsupportedPropertyShape(property.declaration))?;
        ir.statics.push(IrStatic {
            name: property.name.clone(),
            ty: property.storage_ty.unwrap_or(property.ty),
            init,
            is_var: property.flags.has(DeclarationFlags::MUTABLE),
            is_const: property.flags.has(DeclarationFlags::CONST),
            owner: None,
            visibility: property.visibility,
            custom_accessor: property.getter.is_some() || property.setter.is_some(),
            line: 0,
            source_order,
        });
        let getter = property.getter.map(|body| {
            add_accessor_function(
                ir,
                crate::names::property_getter_name(&property.name),
                Vec::new(),
                property.ty,
                body,
                true,
                None,
            )
        });
        let setter = property.setter.map(|body| {
            add_accessor_function(
                ir,
                crate::names::property_setter_name(&property.name),
                vec![property.ty],
                Ty::Unit,
                body,
                false,
                None,
            )
        });
        realizations.insert(
            property_id,
            IrLocalPropertyLayout::TopLevelStorage {
                storage: index,
                getter,
                setter,
                qualifier: None,
            },
        );
        return Ok(());
    }
    let Some(getter_body) = property.getter else {
        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            property.declaration,
        ));
    };
    let receiver_parameters = context_parameters
        .iter()
        .copied()
        .chain(extension_receiver)
        .collect::<Vec<_>>();
    let getter = add_accessor_function(
        ir,
        crate::names::property_getter_name(&property.name),
        receiver_parameters.clone(),
        property.ty,
        getter_body,
        true,
        None,
    );
    let setter = property.setter.map(|body| {
        let mut parameters = receiver_parameters.clone();
        parameters.push(property.ty);
        add_accessor_function(
            ir,
            crate::names::property_setter_name(&property.name),
            parameters,
            Ty::Unit,
            body,
            false,
            None,
        )
    });
    realizations.insert(
        property_id,
        IrLocalPropertyLayout::TopLevelAccessor {
            getter,
            setter,
            receiver: extension_receiver,
            context_parameters,
        },
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn materialize_top_level_delegate(
    source_order: u32,
    property_id: crate::fir::PropertyId,
    mut property: IrCheckedProperty,
    extension_receiver: Option<Ty>,
    context_parameters: Vec<Ty>,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    realizations: &mut HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
) -> Result<(), FirFileLoweringFailure> {
    if !context_parameters.is_empty() {
        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            property.declaration,
        ));
    }
    let initializer =
        property
            .delegate
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let plan =
        property
            .delegate_plan
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let cause = ir.fir_origins.get(&initializer).map(|origin| match origin {
        IrNodeOrigin::Fir(origin) => *origin,
        IrNodeOrigin::Synthetic { cause, .. } => *cause,
    });
    let first_generated = ir.exprs.len();
    let property_reference = delegated_property_reference(
        ir,
        property_id,
        extension_receiver.is_some(),
        property.flags,
    );
    let property_reference_static = push_delegate_static(
        ir,
        format!("{}$kprop", property.name),
        Ty::obj("kotlin/reflect/KProperty"),
        property_reference,
        None,
        source_order,
    )?;
    let owner = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
    let storage_initializer = if let Some(provide) = &plan.provide_delegate {
        let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
        delegated_call(
            index,
            ir,
            provide,
            initializer,
            vec![owner, property_reference],
        )?
    } else {
        initializer
    };
    let delegate_static = push_delegate_static(
        ir,
        format!("{}$delegate", property.name),
        plan.storage_type.get(),
        storage_initializer,
        None,
        source_order,
    )?;
    let receiver = ir.add_expr(IrExpr::GetStatic(delegate_static));
    let owner = if extension_receiver.is_some() {
        ir.add_expr(IrExpr::GetValue(0))
    } else {
        ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null))
    };
    let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
    let read = delegated_call(
        index,
        ir,
        &plan.get_value,
        receiver,
        vec![owner, property_reference],
    )?;
    let getter = add_accessor_function(
        ir,
        crate::names::property_getter_name(&property.name),
        extension_receiver.into_iter().collect(),
        property.ty,
        read,
        true,
        None,
    );
    let setter = plan
        .set_value
        .as_ref()
        .map(|set_value| {
            let receiver = ir.add_expr(IrExpr::GetStatic(delegate_static));
            let owner = if extension_receiver.is_some() {
                ir.add_expr(IrExpr::GetValue(0))
            } else {
                ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null))
            };
            let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
            let value = ir.add_expr(IrExpr::GetValue(u32::from(extension_receiver.is_some())));
            let write = delegated_call(
                index,
                ir,
                set_value,
                receiver,
                vec![owner, property_reference, value],
            )?;
            Ok(add_accessor_function(
                ir,
                crate::names::property_setter_name(&property.name),
                extension_receiver
                    .into_iter()
                    .chain(std::iter::once(property.ty))
                    .collect(),
                Ty::Unit,
                write,
                false,
                None,
            ))
        })
        .transpose()?;
    ir.fn_source_order.insert(getter, source_order);
    if let Some(setter) = setter {
        ir.fn_source_order.insert(setter, source_order);
    }
    realizations.insert(
        property_id,
        IrLocalPropertyLayout::TopLevelAccessor {
            getter,
            setter,
            receiver: extension_receiver,
            context_parameters: Vec::new(),
        },
    );
    stamp_generated_property_nodes(ir, first_generated, cause);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn materialize_member_delegate(
    index: &ResolvedModuleIndex,
    source_order: u32,
    property_id: crate::fir::PropertyId,
    mut property: IrCheckedProperty,
    class_id: crate::ir::ClassId,
    context_parameters: Vec<Ty>,
    ir: &mut IrFile,
    realizations: &mut HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
    initialization: &mut HashMap<crate::ir::ClassId, Vec<(u32, ExprId)>>,
) -> Result<(), FirFileLoweringFailure> {
    if !context_parameters.is_empty() {
        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            property.declaration,
        ));
    }
    let initializer =
        property
            .delegate
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let plan =
        property
            .delegate_plan
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let cause = ir.fir_origins.get(&initializer).map(|origin| match origin {
        IrNodeOrigin::Fir(origin) => *origin,
        IrNodeOrigin::Synthetic { cause, .. } => *cause,
    });
    let first_generated = ir.exprs.len();
    let owner_type = ir.classes[class_id as usize].fq_name;
    let delegate_field = u32::try_from(ir.classes[class_id as usize].fields.len())
        .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
    ir.classes[class_id as usize].fields.push(
        IrField::new(
            format!("{}$delegate", property.name),
            plan.storage_type.get(),
        )
        .with_is_final(true)
        .with_is_private(true),
    );
    let property_reference = delegated_property_reference(ir, property_id, true, property.flags);
    let property_reference_static = push_delegate_static(
        ir,
        format!("{}$kprop", property.name),
        Ty::obj("kotlin/reflect/KProperty"),
        property_reference,
        Some(owner_type),
        source_order,
    )?;
    let this_ref = ir.add_expr(IrExpr::GetValue(0));
    let storage_initializer = if let Some(provide) = &plan.provide_delegate {
        let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
        delegated_call(
            index,
            ir,
            provide,
            initializer,
            vec![this_ref, property_reference],
        )?
    } else {
        initializer
    };
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let store = ir.add_expr(IrExpr::SetField {
        receiver,
        class: class_id,
        index: delegate_field,
        value: storage_initializer,
    });
    ir.property_initializer_stores.insert(store);
    initialization.entry(class_id).or_default().push((
        property
            .initialization_order
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?,
        store,
    ));

    let this_ref = ir.add_expr(IrExpr::GetValue(0));
    let receiver = ir.add_expr(IrExpr::GetField {
        receiver: this_ref,
        class: class_id,
        index: delegate_field,
    });
    let this_ref = ir.add_expr(IrExpr::GetValue(0));
    let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
    let read = delegated_call(
        index,
        ir,
        &plan.get_value,
        receiver,
        vec![this_ref, property_reference],
    )?;
    let getter = add_accessor_function(
        ir,
        crate::names::property_getter_name(&property.name),
        Vec::new(),
        property.ty,
        read,
        true,
        Some(owner_type),
    );
    let setter = plan
        .set_value
        .as_ref()
        .map(|set_value| {
            let this_ref = ir.add_expr(IrExpr::GetValue(0));
            let receiver = ir.add_expr(IrExpr::GetField {
                receiver: this_ref,
                class: class_id,
                index: delegate_field,
            });
            let this_ref = ir.add_expr(IrExpr::GetValue(0));
            let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
            let value = ir.add_expr(IrExpr::GetValue(1));
            let write = delegated_call(
                index,
                ir,
                set_value,
                receiver,
                vec![this_ref, property_reference, value],
            )?;
            Ok(add_accessor_function(
                ir,
                crate::names::property_setter_name(&property.name),
                vec![property.ty],
                Ty::Unit,
                write,
                false,
                Some(owner_type),
            ))
        })
        .transpose()?;
    ir.classes[class_id as usize].methods.push(getter);
    if let Some(setter) = setter {
        ir.classes[class_id as usize].methods.push(setter);
    }
    ir.fn_source_order.insert(getter, source_order);
    if let Some(setter) = setter {
        ir.fn_source_order.insert(setter, source_order);
    }
    let property_index = ir.classes[class_id as usize].properties.len() as u32;
    ir.classes[class_id as usize].properties.push(IrProperty {
        name: property.name.clone(),
        context_params: Vec::new(),
        source_order,
        decl_line: 0,
        ty: property.ty,
        visibility: property.visibility,
        annotations: Box::new([]),
        initializer: None,
        storage_ty: None,
        backing_field: None,
        is_var: property.flags.has(DeclarationFlags::MUTABLE),
        is_open: property.flags.has(DeclarationFlags::OPEN),
        is_private: property.visibility.is_private(),
        setter_is_private: setter_is_private(index, property.declaration),
        getter: Some(getter),
        setter,
        getter_jvm_name: None,
        setter_jvm_name: None,
        needs_access_bridge: false,
    });
    realizations.insert(
        property_id,
        IrLocalPropertyLayout::Member {
            class: class_id,
            owner: owner_type,
            backing_field: None,
            getter: Some(getter),
            setter,
            interface: false,
            name: property.name,
            ty: property.ty,
            mutable: property.flags.has(DeclarationFlags::MUTABLE),
            private: property.visibility.is_private(),
            context_parameters: Vec::new(),
            property: property_index,
        },
    );
    stamp_generated_property_nodes(ir, first_generated, cause);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn materialize_member_extension_delegate(
    index: &ResolvedModuleIndex,
    source_order: u32,
    property_id: crate::fir::PropertyId,
    mut property: IrCheckedProperty,
    class_id: crate::ir::ClassId,
    extension_receiver: Ty,
    context_parameters: Vec<Ty>,
    ir: &mut IrFile,
    realizations: &mut HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
    initialization: &mut HashMap<crate::ir::ClassId, Vec<(u32, ExprId)>>,
) -> Result<(), FirFileLoweringFailure> {
    if !context_parameters.is_empty() || ir.classes[class_id as usize].is_interface {
        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            property.declaration,
        ));
    }
    let initializer =
        property
            .delegate
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let plan =
        property
            .delegate_plan
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let cause = ir.fir_origins.get(&initializer).map(|origin| match origin {
        IrNodeOrigin::Fir(origin) => *origin,
        IrNodeOrigin::Synthetic { cause, .. } => *cause,
    });
    let first_generated = ir.exprs.len();
    let owner = ir.classes[class_id as usize].fq_name;
    let delegate_field = u32::try_from(ir.classes[class_id as usize].fields.len())
        .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
    ir.classes[class_id as usize].fields.push(
        IrField::new(
            format!("{}$delegate", property.name),
            plan.storage_type.get(),
        )
        .with_is_final(true)
        .with_is_private(true),
    );

    // A member extension property has two semantic receivers. Its declaration reference is the
    // unbound KProperty2-like value; the dispatch instance is supplied separately to
    // `provideDelegate`, while accessors supply the extension receiver to getValue/setValue.
    let property_reference = delegated_property_reference(ir, property_id, true, property.flags);
    let property_reference_static = push_delegate_static(
        ir,
        format!("{}$kprop", property.name),
        Ty::obj("kotlin/reflect/KProperty"),
        property_reference,
        Some(owner),
        source_order,
    )?;
    let storage_initializer = if let Some(provide) = &plan.provide_delegate {
        let dispatch = ir.add_expr(IrExpr::GetValue(0));
        let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
        delegated_call(
            index,
            ir,
            provide,
            initializer,
            vec![dispatch, property_reference],
        )?
    } else {
        initializer
    };
    let dispatch = ir.add_expr(IrExpr::GetValue(0));
    let store = ir.add_expr(IrExpr::SetField {
        receiver: dispatch,
        class: class_id,
        index: delegate_field,
        value: storage_initializer,
    });
    ir.property_initializer_stores.insert(store);
    initialization.entry(class_id).or_default().push((
        property
            .initialization_order
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?,
        store,
    ));

    let dispatch = ir.add_expr(IrExpr::GetValue(0));
    let delegate = ir.add_expr(IrExpr::GetField {
        receiver: dispatch,
        class: class_id,
        index: delegate_field,
    });
    let extension = ir.add_expr(IrExpr::GetValue(1));
    let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
    let read = delegated_call(
        index,
        ir,
        &plan.get_value,
        delegate,
        vec![extension, property_reference],
    )?;
    let getter = add_accessor_function(
        ir,
        crate::names::property_getter_name(&property.name),
        vec![extension_receiver],
        property.ty,
        read,
        true,
        Some(owner),
    );
    let setter = plan
        .set_value
        .as_ref()
        .map(|set_value| {
            let dispatch = ir.add_expr(IrExpr::GetValue(0));
            let delegate = ir.add_expr(IrExpr::GetField {
                receiver: dispatch,
                class: class_id,
                index: delegate_field,
            });
            let extension = ir.add_expr(IrExpr::GetValue(1));
            let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
            let value = ir.add_expr(IrExpr::GetValue(2));
            let write = delegated_call(
                index,
                ir,
                set_value,
                delegate,
                vec![extension, property_reference, value],
            )?;
            Ok(add_accessor_function(
                ir,
                crate::names::property_setter_name(&property.name),
                vec![extension_receiver, property.ty],
                Ty::Unit,
                write,
                false,
                Some(owner),
            ))
        })
        .transpose()?;

    let type_params = declaration_type_parameters(index, property.declaration);
    for function in std::iter::once(getter).chain(setter) {
        ir.fn_source_order.insert(function, source_order);
        ir.fresh_method_decls.push(function);
        if property.visibility.is_private() {
            ir.private_methods.insert(function);
        }
        if property.flags.has(DeclarationFlags::OPEN) {
            ir.open_methods.insert(function);
        }
        ir.classes[class_id as usize].methods.push(function);
        if !type_params.is_empty() {
            let signature = &ir.functions[function as usize];
            ir.signatures.insert(
                function,
                crate::ir::IrGenericSig {
                    type_params: type_params.clone(),
                    params: signature.params.clone(),
                    ret: Some(signature.ret),
                    supers: Vec::new(),
                },
            );
        }
    }
    ir.member_ext_props
        .entry(owner)
        .or_default()
        .push(crate::ir::MemberExtProp {
            name: property.name.clone(),
            receiver: extension_receiver,
            ty: property.ty,
            is_var: property.flags.has(DeclarationFlags::MUTABLE),
            is_abstract: false,
            getter,
            setter,
            visibility: property.visibility,
            type_params,
        });
    realizations.insert(
        property_id,
        IrLocalPropertyLayout::MemberExtension {
            owner,
            interface: false,
            name: property.name,
            setter,
            receiver: extension_receiver,
            ty: property.ty,
            context_parameters,
        },
    );
    stamp_generated_property_nodes(ir, first_generated, cause);
    Ok(())
}

fn delegated_property_reference(
    ir: &mut IrFile,
    property: crate::fir::PropertyId,
    unbound: bool,
    flags: DeclarationFlags,
) -> ExprId {
    ir.add_expr(IrExpr::Checked(IrCheckedOperation::PropertyReference {
        target: FirPropertyReferenceTarget::Module(property),
        binding: if unbound {
            FirCallableReferenceBinding::Unbound
        } else {
            FirCallableReferenceBinding::Static
        },
        dispatch_receiver: None,
        extension_receiver: None,
        mutable: flags.has(DeclarationFlags::MUTABLE),
        substitutions: Vec::new(),
        adaptation: None,
    }))
}

fn push_delegate_static(
    ir: &mut IrFile,
    name: String,
    ty: Ty,
    init: ExprId,
    owner: Option<TypeName>,
    source_order: u32,
) -> Result<u32, FirFileLoweringFailure> {
    let index = u32::try_from(ir.statics.len())
        .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
    ir.statics.push(IrStatic {
        name,
        ty,
        init,
        is_var: false,
        is_const: false,
        owner,
        visibility: crate::types::Visibility::Private,
        custom_accessor: true,
        line: 0,
        source_order,
    });
    Ok(index)
}

fn delegated_call(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    call: &FirDelegateCall,
    receiver: ExprId,
    arguments: Vec<ExprId>,
) -> Result<ExprId, FirFileLoweringFailure> {
    match &call.target {
        FirCallTarget::Module(target) => {
            let callable =
                index
                    .callable(*target)
                    .ok_or(FirFileLoweringFailure::MissingCallable(
                        DeclarationId::from_raw(target.raw()),
                    ))?;
            let signature = index.signature(callable.declaration).ok_or(
                FirFileLoweringFailure::MissingCallable(callable.declaration),
            )?;
            let owner = index.enclosing_classifier(callable.declaration);
            if let Some(dispatch) = &call.dispatch_receiver {
                let dispatch = match dispatch {
                    FirDelegateDispatchReceiver::Scoped {
                        current: true,
                        depth: 0,
                        ..
                    } => ir.add_expr(IrExpr::GetValue(0)),
                    FirDelegateDispatchReceiver::Singleton { classifier, .. } => {
                        ir.add_expr(IrExpr::SingletonValue {
                            classifier: *classifier,
                        })
                    }
                    FirDelegateDispatchReceiver::Scoped { .. }
                    | FirDelegateDispatchReceiver::ContextBinding { .. } => {
                        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
                            callable.declaration,
                        ));
                    }
                };
                let owner = owner.ok_or(FirFileLoweringFailure::MissingCallable(
                    callable.declaration,
                ))?;
                let class = ir
                    .checked_classifier_classes
                    .get(&owner.declaration)
                    .copied()
                    .ok_or(FirFileLoweringFailure::MissingClassifier(owner.declaration))?;
                let function = ir.checked_callable_functions.get(target).copied().ok_or(
                    FirFileLoweringFailure::MissingCallable(callable.declaration),
                )?;
                let method = ir.classes[class as usize]
                    .methods
                    .iter()
                    .position(|candidate| *candidate == function)
                    .ok_or(FirFileLoweringFailure::MissingCallable(
                        callable.declaration,
                    ))? as u32;
                let mut arguments = arguments;
                arguments.insert(callable.shape.context_parameter_count as usize, receiver);
                return Ok(ir.add_expr(IrExpr::MethodCall {
                    class,
                    index: method,
                    receiver: dispatch,
                    args: arguments.into_iter().map(Some).collect(),
                }));
            }
            if !call.extension {
                if let Some(owner) = owner {
                    if let (Some(class), Some(function)) = (
                        ir.checked_classifier_classes
                            .get(&owner.declaration)
                            .copied(),
                        ir.checked_callable_functions.get(target).copied(),
                    ) {
                        let method = ir.classes[class as usize]
                            .methods
                            .iter()
                            .position(|candidate| *candidate == function)
                            .ok_or(FirFileLoweringFailure::MissingCallable(
                                callable.declaration,
                            ))? as u32;
                        return Ok(ir.add_expr(IrExpr::MethodCall {
                            class,
                            index: method,
                            receiver,
                            args: arguments.into_iter().map(Some).collect(),
                        }));
                    }
                    return Ok(ir.add_expr(IrExpr::Call {
                        callee: Callee::Virtual {
                            owner: owner.classifier,
                            name: index
                                .callable_name(*target)
                                .ok_or(FirFileLoweringFailure::MissingCallable(
                                    callable.declaration,
                                ))?
                                .to_owned(),
                            descriptor: String::new(),
                            params: Some((
                                signature.parameters.iter().map(|ty| ty.get()).collect(),
                                signature.result.get(),
                            )),
                            interface: index.declaration_header(owner.declaration).is_some_and(
                                |header| header.flags.has(DeclarationFlags::INTERFACE),
                            ),
                        },
                        dispatch_receiver: Some(receiver),
                        args: arguments,
                    }));
                }
            }
            let mut arguments = arguments;
            if call.extension {
                arguments.insert(callable.shape.context_parameter_count as usize, receiver);
            }
            let function = ir.checked_callable_functions.get(target).copied();
            Ok(ir.add_expr(IrExpr::Call {
                callee: function.map_or_else(
                    || Callee::Module {
                        target: *target,
                        name: index.callable_name(*target).unwrap_or_default().to_owned(),
                        params: signature.parameters.iter().map(|ty| ty.get()).collect(),
                        ret: signature.result.get(),
                        default_call: false,
                    },
                    Callee::Local,
                ),
                dispatch_receiver: None,
                args: arguments,
            }))
        }
        FirCallTarget::External {
            declaration,
            receiver: _,
            declared_receiver,
            parameters,
            result,
            declared_result,
            suspend,
            inline_plan: _,
            extension_receiver_parameter,
            ..
        } => {
            let mut arguments = arguments;
            let dispatch_receiver = if let Some(dispatch) = &call.dispatch_receiver {
                let dispatch = match dispatch {
                    FirDelegateDispatchReceiver::Scoped {
                        current: true,
                        depth: 0,
                        ..
                    } => ir.add_expr(IrExpr::GetValue(0)),
                    FirDelegateDispatchReceiver::Singleton { classifier, .. } => {
                        ir.add_expr(IrExpr::SingletonValue {
                            classifier: *classifier,
                        })
                    }
                    FirDelegateDispatchReceiver::Scoped { .. }
                    | FirDelegateDispatchReceiver::ContextBinding { .. } => {
                        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
                            DeclarationId::from_raw(0),
                        ));
                    }
                };
                let parameter = extension_receiver_parameter.ok_or(
                    FirFileLoweringFailure::UnsupportedPropertyShape(DeclarationId::from_raw(0)),
                )? as usize;
                if parameter > arguments.len() {
                    return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
                        DeclarationId::from_raw(0),
                    ));
                }
                arguments.insert(parameter, receiver);
                dispatch
            } else {
                if extension_receiver_parameter.is_some() {
                    return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
                        DeclarationId::from_raw(0),
                    ));
                }
                receiver
            };
            let expression = ir.add_expr(IrExpr::Call {
                callee: Callee::External {
                    target: *declaration,
                    params: parameters.iter().map(|ty| ty.get()).collect(),
                    ret: result.get(),
                    substitutions: Vec::new(),
                    defaults: Vec::new(),
                },
                dispatch_receiver: Some(dispatch_receiver),
                args: arguments,
            });
            if let Some(declared_receiver) = declared_receiver {
                ir.ext_call_source_receiver
                    .insert(expression, declared_receiver.get());
            }
            if let Some(declared_result) = declared_result {
                ir.call_declared_ret
                    .insert(expression, declared_result.get());
            }
            if *suspend {
                ir.suspend_calls.insert(expression, result.get());
            }
            Ok(expression)
        }
        FirCallTarget::Intrinsic { .. }
        | FirCallTarget::Classifier { .. }
        | FirCallTarget::Super { .. } => Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            DeclarationId::from_raw(0),
        )),
    }
}

fn stamp_generated_property_nodes(
    ir: &mut IrFile,
    first: usize,
    cause: Option<crate::fir::OriginId>,
) {
    let Some(cause) = cause else {
        return;
    };
    for raw in first..ir.exprs.len() {
        ir.fir_origins
            .entry(raw as u32)
            .or_insert(IrNodeOrigin::Synthetic {
                cause,
                kind: crate::fir::SyntheticOriginKind::GeneratedAccessor,
            });
    }
}

#[allow(clippy::too_many_arguments)]
fn materialize_member_property(
    index: &ResolvedModuleIndex,
    anchor: crate::fir::StableDeclarationAnchor,
    source_order: u32,
    property_id: crate::fir::PropertyId,
    property: IrCheckedProperty,
    class_id: crate::ir::ClassId,
    context_parameters: Vec<Ty>,
    ir: &mut IrFile,
    realizations: &mut HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
    initialization: &mut HashMap<crate::ir::ClassId, Vec<(u32, ExprId)>>,
) -> Result<(), FirFileLoweringFailure> {
    let owner = ir.classes[class_id as usize].fq_name;
    let needs_property_reference_bridge = (property.visibility.is_private()
        || setter_is_private(index, property.declaration))
        && ir.exprs.iter().any(|expression| {
            matches!(
                expression,
                IrExpr::Checked(IrCheckedOperation::PropertyReference {
                    target,
                    ..
                }) if target.module() == Some(property_id)
            )
        });
    let class_flags = anchor
        .owner
        .and_then(|owner| index.declaration_header(owner))
        .map(|header| header.flags)
        .unwrap_or_default();
    if property.delegate.is_some() || property.delegate_plan.is_some() {
        return materialize_member_delegate(
            index,
            source_order,
            property_id,
            property,
            class_id,
            context_parameters,
            ir,
            realizations,
            initialization,
        );
    }
    // A checked member `const val` is necessarily declared on an object/companion. Its common-IR
    // declaration is static storage owned by that semantic classifier, not an instance property:
    // the JVM pass may subsequently move a companion constant to its outer class, while other
    // backends retain the same semantic owner. Keeping it as an `IrProperty` would manufacture an
    // instance backing field/getter and leave the backend's selected static reads without a field.
    if property.flags.has(DeclarationFlags::CONST) {
        if !class_flags.has(DeclarationFlags::SINGLETON)
            || !context_parameters.is_empty()
            || property.flags.has(DeclarationFlags::MUTABLE)
        {
            return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ));
        }
        let initializer =
            property
                .initializer
                .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                    property.declaration,
                ))?;
        let static_id = u32::try_from(ir.statics.len())
            .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
        ir.statics.push(IrStatic {
            name: property.name,
            ty: property.ty,
            init: initializer,
            is_var: false,
            is_const: true,
            owner: Some(owner),
            visibility: property.visibility,
            custom_accessor: false,
            line: 0,
            source_order,
        });
        ir.declared_class_statics
            .entry(owner)
            .or_default()
            .push(static_id);
        realizations.insert(
            property_id,
            IrLocalPropertyLayout::TopLevelStorage {
                storage: static_id,
                getter: None,
                setter: None,
                qualifier: Some(owner),
            },
        );
        return Ok(());
    }
    let has_storage = !class_flags.has(DeclarationFlags::INTERFACE)
        && property.delegate.is_none()
        && (property.flags.has(DeclarationFlags::PROPERTY_PARAMETER)
            || property.initializer.is_some()
            || property.flags.has(DeclarationFlags::LATEINIT)
            || property.flags.has(DeclarationFlags::EXPLICIT_BACKING_FIELD)
            || property
                .flags
                .has(DeclarationFlags::GETTER_READS_BACKING_FIELD)
            || (!property.flags.has(DeclarationFlags::CUSTOM_GETTER)
                && !property.flags.has(DeclarationFlags::ABSTRACT)));
    if has_storage && !context_parameters.is_empty() {
        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            property.declaration,
        ));
    }
    let backing_field = if has_storage {
        let class = &ir.classes[class_id as usize];
        let has_default = if property.flags.has(DeclarationFlags::PROPERTY_PARAMETER) {
            let classifier = anchor.owner.ok_or(FirFileLoweringFailure::MissingProperty(
                property.declaration,
            ))?;
            let source_parameter_count = (0..index.declaration_count())
                .find_map(|raw| {
                    let constructor = DeclarationId::from_raw(raw as u32);
                    index.declaration_anchor(constructor).and_then(|candidate| {
                        (candidate.kind == DeclarationKind::Constructor
                            && candidate.owner == Some(classifier)
                            && candidate.sibling == 0)
                            .then(|| index.signature(constructor).map(|sig| sig.parameters.len()))
                            .flatten()
                    })
                })
                .ok_or(FirFileLoweringFailure::MissingProperty(
                    property.declaration,
                ))?;
            let capture_count = class
                .ctor_args
                .len()
                .checked_sub(source_parameter_count)
                .ok_or(FirFileLoweringFailure::MissingProperty(
                    property.declaration,
                ))?;
            let parameter = class
                .ctor_args
                .get(capture_count + anchor.sibling as usize)
                .ok_or(FirFileLoweringFailure::MissingProperty(
                    property.declaration,
                ))?;
            if !parameter.is_field
                || parameter.name.as_deref() != Some(property.name.as_str())
                || parameter.ty != property.ty
            {
                crate::trace_compiler!(
                    "lower",
                    "property parameter layout mismatch declaration={:?} property={} ty={:?} sibling={} source_parameters={} captures={} constructor_argument={parameter:?}",
                    property.declaration,
                    property.name,
                    property.ty,
                    anchor.sibling,
                    source_parameter_count,
                    capture_count,
                );
                return Err(FirFileLoweringFailure::MissingProperty(
                    property.declaration,
                ));
            }
            parameter.has_default
        } else {
            false
        };
        // A synthesized class's CAPTURE already owns a field, spliced in with its constructor
        // argument when the class was lowered. `install_anonymous_object_captures` also registers
        // each capture as a synthetic property, so emitting a field for that property again gives
        // the class two identically-named fields and the JVM refuses to load it
        // (`ClassFormatError: Duplicate field name`). Reuse the existing field instead.
        let storage_ty = property.storage_ty.unwrap_or(property.ty);
        let existing = ir.classes[class_id as usize]
            .fields
            .iter()
            .position(|field| field.name == property.name && field.ty == storage_ty);
        let field = u32::try_from(existing.unwrap_or(ir.classes[class_id as usize].fields.len()))
            .map_err(|_| {
            FirFileLoweringFailure::UnsupportedPropertyShape(property.declaration)
        })?;
        if existing.is_none() {
            let field_type_parameter = anchor
                .owner
                .and_then(|classifier| {
                    classifier_type_parameter_ordinal(index, classifier, storage_ty)
                        .and_then(|ordinal| index.type_parameter(classifier, ordinal))
                })
                .and_then(|parameter| index.type_parameter_name(parameter))
                .map(str::to_owned);
            ir.classes[class_id as usize].fields.push(IrField {
                name: property.name.clone(),
                ty: storage_ty,
                type_param: field_type_parameter,
                default: None,
                flags: IrfFlags::default()
                    .with_has_default(has_default)
                    .with_is_final(!property.flags.has(DeclarationFlags::MUTABLE))
                    .with_is_private(true)
                    .with_is_lateinit(property.flags.has(DeclarationFlags::LATEINIT)),
            });
        }
        if property.flags.has(DeclarationFlags::PROPERTY_PARAMETER) {
            ir.classes[class_id as usize].ctor_param_count += 1;
        }
        if let Some(initializer) = property.initializer {
            let receiver = ir.add_expr(IrExpr::GetValue(0));
            let store = ir.add_expr(IrExpr::SetField {
                receiver,
                class: class_id,
                index: field,
                value: initializer,
            });
            ir.property_initializer_stores.insert(store);
            initialization.entry(class_id).or_default().push((
                property.initialization_order.ok_or(
                    FirFileLoweringFailure::UnsupportedPropertyShape(property.declaration),
                )?,
                store,
            ));
        }
        Some(field)
    } else {
        None
    };

    let getter = property.getter.map(|body| {
        let function = add_accessor_function(
            ir,
            crate::names::property_getter_name(&property.name),
            context_parameters.clone(),
            property.ty,
            body,
            true,
            Some(owner),
        );
        ir.classes[class_id as usize].methods.push(function);
        function
    });
    // An abstract property has no FIR body and no backing field, but its accessor declarations are
    // still part of the class ABI. Publish those methods explicitly in common IR so every backend
    // sees the same checked declaration shape; a backend must not infer them from a property name.
    if backing_field.is_none()
        && property.getter.is_none()
        && (class_flags.has(DeclarationFlags::INTERFACE)
            || property.flags.has(DeclarationFlags::ABSTRACT))
    {
        let getter = add_abstract_accessor_function(
            ir,
            crate::names::property_getter_name(&property.name),
            context_parameters.clone(),
            property.ty,
            owner,
        );
        ir.fn_source_order.insert(getter, source_order);
        ir.open_methods.insert(getter);
        ir.classes[class_id as usize].methods.push(getter);
        if property.flags.has(DeclarationFlags::MUTABLE) {
            let setter = add_abstract_accessor_function(
                ir,
                crate::names::property_setter_name(&property.name),
                context_parameters
                    .iter()
                    .copied()
                    .chain(std::iter::once(property.ty))
                    .collect(),
                Ty::Unit,
                owner,
            );
            ir.fn_source_order.insert(setter, source_order);
            ir.open_methods.insert(setter);
            ir.classes[class_id as usize].methods.push(setter);
        }
    }
    let setter = property.setter.map(|body| {
        let function = add_accessor_function(
            ir,
            crate::names::property_setter_name(&property.name),
            context_parameters
                .iter()
                .copied()
                .chain(std::iter::once(property.ty))
                .collect(),
            Ty::Unit,
            body,
            false,
            Some(owner),
        );
        ir.classes[class_id as usize].methods.push(function);
        function
    });
    let property_index = ir.classes[class_id as usize].properties.len() as u32;
    ir.classes[class_id as usize].properties.push(IrProperty {
        name: property.name.clone(),
        context_params: named_context_parameters(index, property.declaration, &context_parameters),
        source_order,
        decl_line: 0,
        ty: property.ty,
        visibility: property.visibility,
        annotations: index
            .declaration_annotations(property.declaration)
            .to_vec()
            .into_boxed_slice(),
        initializer: property.initializer,
        storage_ty: property.storage_ty,
        backing_field,
        is_var: property.flags.has(DeclarationFlags::MUTABLE),
        is_open: property.flags.has(DeclarationFlags::OPEN),
        is_private: property.visibility.is_private(),
        setter_is_private: setter_is_private(index, property.declaration),
        getter,
        setter,
        getter_jvm_name: None,
        setter_jvm_name: None,
        needs_access_bridge: needs_property_reference_bridge,
    });
    realizations.insert(
        property_id,
        IrLocalPropertyLayout::Member {
            class: class_id,
            owner,
            backing_field,
            getter,
            setter,
            interface: class_flags.has(DeclarationFlags::INTERFACE),
            name: property.name,
            ty: property.ty,
            mutable: property.flags.has(DeclarationFlags::MUTABLE),
            private: property.visibility.is_private(),
            context_parameters,
            property: property_index,
        },
    );
    Ok(())
}

fn add_accessor_function(
    ir: &mut IrFile,
    name: String,
    params: Vec<Ty>,
    ret: Ty,
    value: ExprId,
    returns_value: bool,
    dispatch_receiver: Option<TypeName>,
) -> FunId {
    let returned = ir.add_expr(IrExpr::Return(returns_value.then_some(value)));
    let body = if returns_value {
        ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        })
    } else {
        ir.add_expr(IrExpr::Block {
            stmts: vec![value, returned],
            value: None,
        })
    };
    let parameter_count = params.len();
    let function = ir.add_fun(IrFunction {
        name,
        params,
        ret,
        body: Some(body),
        is_static: dispatch_receiver.is_none(),
        dispatch_receiver,
        param_checks: vec![None; parameter_count],
    });
    ir.fn_params.insert(
        function,
        crate::ir::FnParamInfo::names(
            (0..parameter_count)
                .map(|ordinal| format!("value{ordinal}"))
                .collect(),
        ),
    );
    function
}

fn add_abstract_accessor_function(
    ir: &mut IrFile,
    name: String,
    params: Vec<Ty>,
    ret: Ty,
    owner: TypeName,
) -> FunId {
    let parameter_count = params.len();
    let function = ir.add_fun(IrFunction {
        name,
        params,
        ret,
        body: None,
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks: vec![None; parameter_count],
    });
    ir.fn_params.insert(
        function,
        crate::ir::FnParamInfo::names(
            (0..parameter_count)
                .map(|ordinal| format!("value{ordinal}"))
                .collect(),
        ),
    );
    function
}

fn setter_is_private(index: &ResolvedModuleIndex, declaration: DeclarationId) -> bool {
    (0..index.declaration_count()).any(|raw| {
        let accessor = DeclarationId::from_raw(raw as u32);
        index.declaration_anchor(accessor).is_some_and(|anchor| {
            anchor.kind == DeclarationKind::Accessor
                && anchor.owner == Some(declaration)
                && anchor.sibling == 1
        }) && index
            .declaration_header(accessor)
            .is_some_and(|header| header.visibility.is_private())
    })
}

fn merge_class_initialization(
    ir: &mut IrFile,
    mut initialization: HashMap<crate::ir::ClassId, Vec<(u32, ExprId)>>,
) -> Result<(), FirFileLoweringFailure> {
    for initializer in &ir.checked_class_initializers {
        initialization
            .entry(initializer.class)
            .or_default()
            .push((initializer.initialization_order, initializer.body));
    }
    for (class_id, mut steps) in initialization {
        if let Some(body) = ir.classes[class_id as usize].init_body.take() {
            steps.push((u32::MAX, body));
        }
        steps.sort_by_key(|(order, _)| *order);
        let initialization = ir.add_expr(IrExpr::Block {
            stmts: steps
                .into_iter()
                .map(|(_, expression)| expression)
                .collect(),
            value: None,
        });
        ir.classes[class_id as usize].init_body = Some(initialization);

        // With no primary constructor, Kotlin runs property initializers and `init` blocks in the
        // secondary constructor that delegates directly to `super`, before that constructor's own
        // body. A `this(...)` secondary must not repeat them: its eventual direct-super target owns
        // the single execution. Encode that ordering into the common constructor bodies instead of
        // asking a backend to rediscover it from constructor shape.
        if !ir.classes[class_id as usize].has_primary_ctor {
            let constructor_value_count = ir.classes[class_id as usize]
                .secondary_ctors
                .iter()
                .map(|constructor| constructor.prefix_params.len() + constructor.params.len())
                .max()
                .unwrap_or(0);
            let constructor_value_count = u32::try_from(constructor_value_count)
                .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
            rebase_initializer_values(ir, initialization, constructor_value_count)?;
            let direct_super_bodies = ir.classes[class_id as usize]
                .secondary_ctors
                .iter()
                .enumerate()
                .filter_map(|(ordinal, constructor)| {
                    matches!(
                        constructor.delegate,
                        crate::ir::CtorDelegateTarget::Super { .. }
                    )
                    .then_some((ordinal, constructor.body))
                })
                .collect::<Vec<_>>();
            for (ordinal, body) in direct_super_bodies {
                let body = ir.add_expr(IrExpr::Block {
                    stmts: std::iter::once(initialization).chain(body).collect(),
                    value: None,
                });
                ir.classes[class_id as usize].secondary_ctors[ordinal].body = Some(body);
            }
        }
    }
    Ok(())
}

/// Body-local value identities restart in each streamed FIR unit. Once a class initializer is
/// composed into a secondary constructor, reserve the constructor's parameter prefix (`this` stays
/// value 0) and move every initializer-local identity above it.
fn rebase_initializer_values(
    ir: &mut IrFile,
    root: ExprId,
    offset: u32,
) -> Result<(), FirFileLoweringFailure> {
    if offset == 0 {
        return Ok(());
    }
    let mut seen = std::collections::HashSet::new();
    let mut pending = vec![root];
    let mut expressions = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        expressions.push(expression);
    }
    let rebase = |value: &mut u32| -> Result<(), FirFileLoweringFailure> {
        if *value != 0 {
            *value = value
                .checked_add(offset)
                .ok_or(FirFileLoweringFailure::ValueIdentityOverflow)?;
        }
        Ok(())
    };
    for expression in expressions {
        match &mut ir.exprs[expression as usize] {
            IrExpr::Variable { index, .. } | IrExpr::GetValue(index) => rebase(index)?,
            IrExpr::SetValue { var, .. } => rebase(var)?,
            IrExpr::Try { catches, .. } => {
                for catch in catches {
                    rebase(&mut catch.var)?;
                }
            }
            IrExpr::Checked(crate::ir::IrCheckedOperation::RangeLoop { variable, .. }) => {
                rebase(variable)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn realize_backing_field_operations(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    realizations: &HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
) -> Result<(), FirFileLoweringFailure> {
    for raw in 0..ir.exprs.len() {
        let operation = match ir.exprs[raw].clone() {
            IrExpr::Checked(operation) => operation,
            _ => continue,
        };
        let replacement = match operation {
            IrCheckedOperation::LateinitFieldRead { target } => {
                lateinit_field_read(ir, realizations.get(&target), index, target)?
            }
            IrCheckedOperation::BackingFieldRead { target } => {
                backing_field_read(ir, realizations.get(&target), index, target)?
            }
            IrCheckedOperation::BackingFieldWrite { target, value } => {
                backing_field_write(ir, realizations.get(&target), value, index, target)?
            }
            _ => continue,
        };
        ir.exprs[raw] = replacement;
    }
    Ok(())
}

fn backing_field_read(
    ir: &mut IrFile,
    realization: Option<&IrLocalPropertyLayout>,
    index: &ResolvedModuleIndex,
    target: crate::fir::PropertyId,
) -> Result<IrExpr, FirFileLoweringFailure> {
    match realization {
        Some(IrLocalPropertyLayout::TopLevelStorage { storage, .. }) => {
            Ok(IrExpr::GetStatic(*storage))
        }
        Some(IrLocalPropertyLayout::Member {
            class,
            backing_field: Some(field),
            ..
        }) => {
            let receiver = ir.add_expr(IrExpr::GetValue(0));
            Ok(IrExpr::GetField {
                receiver,
                class: *class,
                index: *field,
            })
        }
        _ => Err(property_failure(index, target)),
    }
}

/// The RAW field read behind `::v.isInitialized`. `IrExpr::LateinitInitialized` is the existing
/// primitive the old lowering used for exactly this: it reads the field without the
/// uninitialized-access guard that an ordinary `lateinit` read carries.
fn lateinit_field_read(
    ir: &mut IrFile,
    realization: Option<&IrLocalPropertyLayout>,
    index: &ResolvedModuleIndex,
    target: crate::fir::PropertyId,
) -> Result<IrExpr, FirFileLoweringFailure> {
    match realization {
        Some(IrLocalPropertyLayout::TopLevelStorage { storage, .. }) => {
            Ok(IrExpr::GetStatic(*storage))
        }
        Some(IrLocalPropertyLayout::Member {
            class,
            backing_field: Some(field),
            ..
        }) => {
            let receiver = ir.add_expr(IrExpr::GetValue(0));
            Ok(IrExpr::LateinitInitialized {
                receiver,
                class: *class,
                index: *field,
            })
        }
        _ => Err(property_failure(index, target)),
    }
}

fn backing_field_write(
    ir: &mut IrFile,
    realization: Option<&IrLocalPropertyLayout>,
    value: ExprId,
    index: &ResolvedModuleIndex,
    target: crate::fir::PropertyId,
) -> Result<IrExpr, FirFileLoweringFailure> {
    match realization {
        Some(IrLocalPropertyLayout::TopLevelStorage { storage, .. }) => Ok(IrExpr::SetStatic {
            index: *storage,
            value,
        }),
        Some(IrLocalPropertyLayout::Member {
            class,
            backing_field: Some(field),
            ..
        }) => {
            let receiver = ir.add_expr(IrExpr::GetValue(0));
            Ok(IrExpr::SetField {
                receiver,
                class: *class,
                index: *field,
                value,
            })
        }
        _ => Err(property_failure(index, target)),
    }
}

fn property_failure(
    index: &ResolvedModuleIndex,
    property: crate::fir::PropertyId,
) -> FirFileLoweringFailure {
    FirFileLoweringFailure::UnsupportedPropertyShape(
        index
            .property(property)
            .map_or(DeclarationId::from_raw(property.raw()), |header| {
                header.declaration
            }),
    )
}

pub(super) fn accept_property_body(
    declaration: DeclarationId,
    body: FirBody,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    local_callables: &mut LocalCallableLoweringContext,
) -> Result<(), FirFileLoweringFailure> {
    let anchor = index
        .declaration_anchor(declaration)
        .ok_or(FirFileLoweringFailure::MissingProperty(declaration))?;
    let property_declaration = if anchor.kind == DeclarationKind::Accessor {
        anchor
            .owner
            .ok_or(FirFileLoweringFailure::MissingProperty(declaration))?
    } else {
        declaration
    };
    let property_id = index.property_for_declaration(property_declaration).ok_or(
        FirFileLoweringFailure::MissingProperty(property_declaration),
    )?;
    let origin = body
        .roots()
        .first()
        .and_then(|root| body.statement(*root))
        .map(|statement| statement.origin);
    let lowered = lower_body_with_context(body, index, ir, local_callables)
        .map_err(FirFileLoweringFailure::Body)?;
    if !lowered.defaults.is_empty() {
        return Err(FirFileLoweringFailure::MissingProperty(
            property_declaration,
        ));
    }
    let value = body_value(lowered.roots.into_vec(), origin, ir)?;
    let property = ir.checked_properties.get_mut(&property_id).ok_or(
        FirFileLoweringFailure::MissingProperty(property_declaration),
    )?;
    if let Some(storage) = lowered.property_storage_type {
        if property.storage_ty.replace(storage).is_some() {
            return Err(FirFileLoweringFailure::MissingProperty(declaration));
        }
    }
    if property.flags.has(DeclarationFlags::DELEGATED) {
        let plan =
            lowered
                .property_delegate
                .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                    property_declaration,
                ))?;
        if property.delegate_plan.replace(plan).is_some() {
            return Err(FirFileLoweringFailure::MissingProperty(declaration));
        }
    } else if lowered.property_delegate.is_some() {
        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            property_declaration,
        ));
    }
    let slot = if anchor.kind == DeclarationKind::Accessor {
        if anchor.sibling == 0 {
            &mut property.getter
        } else if anchor.sibling == 1 {
            &mut property.setter
        } else {
            return Err(FirFileLoweringFailure::MissingProperty(declaration));
        }
    } else if property.flags.has(DeclarationFlags::DELEGATED) {
        &mut property.delegate
    } else {
        &mut property.initializer
    };
    if slot.replace(value).is_some() {
        return Err(FirFileLoweringFailure::MissingProperty(declaration));
    }
    Ok(())
}

fn body_value(
    mut roots: Vec<crate::ir::ExprId>,
    origin: Option<crate::fir::OriginId>,
    ir: &mut IrFile,
) -> Result<crate::ir::ExprId, FirFileLoweringFailure> {
    let value = roots.pop().ok_or(FirFileLoweringFailure::Body(
        super::FirLoweringFailure::MissingBodyResult {
            origin: origin.unwrap_or(crate::fir::OriginId::from_raw(0)),
        },
    ))?;
    if roots.is_empty() {
        return Ok(value);
    }
    let first = ir.exprs.len();
    let block = ir.add_expr(IrExpr::Block {
        stmts: roots,
        value: Some(value),
    });
    if let Some(cause) = origin {
        for raw in first..ir.exprs.len() {
            ir.fir_origins.insert(
                raw as u32,
                IrNodeOrigin::Synthetic {
                    cause,
                    kind: crate::fir::SyntheticOriginKind::GeneratedControlFlow,
                },
            );
        }
    }
    Ok(block)
}
