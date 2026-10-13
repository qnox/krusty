//! JVM declaration-signature validation after representation selection.
//!
//! Kotlin declarations can be distinct at the source level while their JVM realizations collide.
//! This pass runs only after representation passes have selected physical method names and types,
//! so FIR and common lowering never depend on JVM descriptors.

use crate::diag::{DiagSink, Span};
use crate::ir::{FunId, IrFile};

pub(super) fn validate(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    delegated_arrays: &crate::jvm::property_references::DelegatedPropertyArrays,
    diags: &mut DiagSink,
) -> Result<(), ()> {
    let platform_clash = report_platform_declaration_clashes(ir, override_results, diags)
        | report_jvm_field_companion_alias_clashes(ir, diags)
        | report_jvm_field_enum_entry_clashes(ir, diags)
        | report_jvm_field_companion_block_const_clashes(ir, diags)
        | report_delegated_property_array_clashes(ir, delegated_arrays, diags);
    for (class_id, class) in ir.classes.iter().enumerate() {
        if let Err(message) = reject_duplicate_constructors(class) {
            diags.error(Span::new(0, 0), message);
            return Err(());
        }
        let Some(properties) = ir.member_ext_props.get(&class.fq_name) else {
            continue;
        };
        for property in properties {
            for (accessor, span) in [
                (Some(property.getter), property.getter_diagnostic_span),
                (property.setter, property.setter_diagnostic_span),
            ] {
                let Some(accessor) = accessor else { continue };
                // The exact inventory owns every declaration with semantic provenance. Keep this
                // representation fallback only for compiler-generated methods which have not yet
                // published such provenance; otherwise a marker/accessor clash would be reported a
                // second time as a generic Span(0, 0) error.
                if let Err(message) =
                    reject_uninventoried_duplicate_method(ir, class_id as u32, class, accessor)
                {
                    diags.error(
                        span.or(property.diagnostic_span)
                            .unwrap_or_else(|| Span::new(0, 0)),
                        message,
                    );
                    return Err(());
                }
            }
        }
    }
    let package_clashes = match package_function_signature_clashes(ir) {
        Ok(clashes) => clashes,
        Err(errors) => {
            for (span, message) in errors {
                diags.error(span, message);
            }
            return Err(());
        }
    };
    let package_clash = !package_clashes.is_empty();
    for (span, message) in package_clashes {
        diags.error(span, message);
    }
    if platform_clash || package_clash {
        Err(())
    } else {
        Ok(())
    }
}

/// A generated delegated-property array is itself a physical field declaration. Once a public
/// source field has displaced it to `$N`, an ordinary companion property with that exact allocated
/// signature is a platform clash; private-storage suffixing must not silently erase the source
/// declaration Kotlin reports.
fn report_delegated_property_array_clashes(
    ir: &IrFile,
    delegated_arrays: &crate::jvm::property_references::DelegatedPropertyArrays,
    diags: &mut DiagSink,
) -> bool {
    let mut clashes = Vec::new();
    for (class_id, class) in ir.classes.iter().enumerate() {
        if delegated_arrays.elements(class.fq_name).is_none() {
            continue;
        }
        let field_name = delegated_arrays.field_name(class.fq_name);
        for (index, field) in ir.statics.iter().enumerate() {
            if field.owner != Some(class.fq_name)
                || ir.static_field_jvm_name(index as u32) != field_name
                || crate::jvm::names::type_descriptor(field.ty)
                    != crate::jvm::property_references::delegated_arrays::DESCRIPTOR
                || ir.is_jvm_field_static(index as u32)
            {
                continue;
            }
            let Some((_, property)) = ir.jvm_companion_static_declaration(index as u32) else {
                continue;
            };
            let (Some(class_span), Some(property_span)) = (
                ir.class_diagnostic_span(class_id as u32),
                property.diagnostic_span,
            ) else {
                continue;
            };
            let Ok(property_ty) = source_type_name(property.ty) else {
                continue;
            };
            let owner = class.fq_name.render().replace(['/', '$'], ".");
            let kind = if property.is_var { "var" } else { "val" };
            let declaration = format!(
                "{kind} `{}`: {property_ty} defined in {owner}",
                property.name
            );
            let generated =
                format!("val `{field_name}`: Array<out KProperty<out Any?>> defined in {owner}");
            let message = clash_message(
                field_name,
                crate::jvm::property_references::delegated_arrays::DESCRIPTOR,
                &[declaration, generated],
            );
            clashes.push((class_span.lo, message.clone()));
            clashes.push((property_span.lo, message));
        }
    }
    clashes.sort_by_key(|(offset, _)| *offset);
    let found = !clashes.is_empty();
    for (offset, message) in clashes {
        diags.error(Span::new(offset, offset), message);
    }
    found
}

/// A companion property's public `@JvmField` surface cannot be renamed away from an enclosing
/// class's synthetic `Companion` alias. A same-signature declaration is therefore a platform
/// declaration clash, unlike ordinary private companion backing storage, which kotlinc is free to
/// disambiguate with a `$N` physical name.
fn report_jvm_field_companion_alias_clashes(ir: &IrFile, diags: &mut DiagSink) -> bool {
    let mut clashes = Vec::new();
    for (class_id, class) in ir.classes.iter().enumerate() {
        let Some(companion) = class.companion_class else {
            continue;
        };
        let name = companion.nested_segment_ref();
        let descriptor = format!("L{};", companion.render());
        for (index, _) in ir.statics.iter().enumerate().filter(|(_, storage)| {
            storage.owner == Some(class.fq_name)
                && storage.name == name
                && crate::jvm::names::type_descriptor(storage.ty) == descriptor
        }) {
            if !ir.is_jvm_field_static(index as u32) {
                continue;
            }
            let Some((_, property)) = ir.jvm_companion_static_declaration(index as u32) else {
                continue;
            };
            let Ok(ty) = source_type_name(property.ty) else {
                continue;
            };
            let owner = class.fq_name.render().replace(['/', '$'], ".");
            let kind = if property.is_var { "var" } else { "val" };
            let declaration = format!("{kind} {}: {ty} defined in {owner}", property.name);
            let message = format!(
                "platform declaration clash: The following declarations have the same JVM signature ({name}{descriptor}):\n    {declaration}\n    {declaration}"
            );
            let class_span = ir
                .class_diagnostic_span(class_id as u32)
                .expect("a source class with a companion retains its exact diagnostic span");
            clashes.push((class_span.lo, message.clone()));
            let span = property
                .diagnostic_span
                .expect("a source companion @JvmField retains its exact diagnostic span");
            clashes.push((span.lo, message));
        }
    }
    clashes.sort_by_key(|(offset, _)| *offset);
    let found = !clashes.is_empty();
    for (offset, message) in clashes {
        diags.error(Span::new(offset, offset), message);
    }
    found
}

/// An enum entry and a companion `@JvmField` are both public static fields on the enum class. When
/// their full JVM signatures match, neither declaration may receive the private-storage `$N`
/// disambiguation: both spellings are part of the Java-facing ABI.
fn report_jvm_field_enum_entry_clashes(ir: &IrFile, diags: &mut DiagSink) -> bool {
    let mut clashes = Vec::new();
    for class in ir.classes.iter().filter(|class| class.is_enum) {
        let descriptor = format!("L{};", class.fq_name.render());
        let owner = class.fq_name.render().replace(['/', '$'], ".");
        for (index, field) in ir.statics.iter().enumerate().filter(|(_, field)| {
            field.owner == Some(class.fq_name)
                && crate::jvm::names::type_descriptor(field.ty) == descriptor
        }) {
            if !ir.is_jvm_field_static(index as u32) {
                continue;
            }
            let Some((_, property)) = ir.jvm_companion_static_declaration(index as u32) else {
                continue;
            };
            let Some(entry) = class
                .enum_entries
                .iter()
                .find(|entry| entry.name == field.name)
            else {
                continue;
            };
            let (Some(entry_span), Some(property_span)) =
                (entry.diagnostic_span, property.diagnostic_span)
            else {
                continue;
            };
            let Ok(ty) = source_type_name(property.ty) else {
                continue;
            };
            let kind = if property.is_var { "var" } else { "val" };
            let mut declarations = vec![
                (
                    entry_span,
                    format!("val {}: {ty} defined in {owner}", entry.name),
                ),
                (
                    property_span,
                    format!("{kind} {}: {ty} defined in {owner}", property.name),
                ),
            ];
            declarations.sort_by_key(|(span, _)| span.lo);
            let rendered = declarations
                .iter()
                .map(|(_, declaration)| declaration.clone())
                .collect::<Vec<_>>();
            let message = clash_message(&field.name, &descriptor, &rendered);
            clashes.extend(
                declarations
                    .into_iter()
                    .map(|(span, _)| (span.lo, message.clone())),
            );
        }
    }
    clashes.sort_by_key(|(offset, _)| *offset);
    let found = !clashes.is_empty();
    for (offset, message) in clashes {
        diags.error(Span::new(offset, offset), message);
    }
    found
}

/// A companion-block `const val` and a companion-object `@JvmField` are both public fields on the
/// enclosing class. Neither public ABI may receive the backing-storage `$N` disambiguation used
/// for an ordinary private block field, so an exact physical-signature collision is rejected.
fn report_jvm_field_companion_block_const_clashes(ir: &IrFile, diags: &mut DiagSink) -> bool {
    let mut clashes = Vec::new();
    for (jvm_field_index, jvm_field) in ir.statics.iter().enumerate() {
        if !ir.is_jvm_field_static(jvm_field_index as u32) {
            continue;
        }
        let Some(owner) = jvm_field.owner else {
            continue;
        };
        let descriptor = crate::jvm::names::type_descriptor(jvm_field.ty);
        let Some((_, property)) = ir.jvm_companion_static_declaration(jvm_field_index as u32)
        else {
            continue;
        };
        let Some(property_span) = property.diagnostic_span else {
            continue;
        };
        let Some(block) = ir.companion_blocks.properties_of(owner).find(|block| {
            block.is_const
                && block.storage.is_some_and(|storage| {
                    let storage = &ir.statics[storage as usize];
                    storage.name == jvm_field.name
                        && crate::jvm::names::type_descriptor(storage.ty) == descriptor
                })
        }) else {
            continue;
        };
        let Some(block_span) = block.diagnostic_span else {
            continue;
        };
        let (Ok(block_ty), Ok(property_ty)) =
            (source_type_name(block.ty), source_type_name(property.ty))
        else {
            continue;
        };
        let outer = owner.render().replace(['/', '$'], ".");
        let block_kind = if block.is_var { "var" } else { "val" };
        let property_kind = if property.is_var { "var" } else { "val" };
        let mut declarations = vec![
            (
                block_span,
                format!("{block_kind} {}: {block_ty} defined in {outer}", block.name),
            ),
            (
                property_span,
                format!(
                    "{property_kind} {}: {property_ty} defined in {outer}",
                    property.name
                ),
            ),
        ];
        declarations.sort_by_key(|(span, _)| span.lo);
        let rendered = declarations
            .iter()
            .map(|(_, declaration)| declaration.clone())
            .collect::<Vec<_>>();
        let message = clash_message(&jvm_field.name, &descriptor, &rendered);
        clashes.extend(
            declarations
                .into_iter()
                .map(|(span, _)| (span.lo, message.clone())),
        );
    }
    clashes.sort_by_key(|(offset, _)| *offset);
    let found = !clashes.is_empty();
    for (offset, message) in clashes {
        diags.error(Span::new(offset, offset), message);
    }
    found
}

/// Source methods whose physical JVM name and descriptor are identical. Kotlin already rejected
/// members that share a declaration signature, so a group here differs in Kotlin and collides only
/// after erasure. A generated property-annotation marker is the one representation that must join
/// these groups: it occupies the class method namespace and kotlinc reports its clash at the class
/// header as well as at each colliding source method.
#[derive(Clone, Copy)]
enum MemberMethodDeclaration {
    Source {
        offset: u32,
        function: FunId,
    },
    GeneratedDataMember {
        offset: u32,
        function: FunId,
    },
    PropertyAnnotationMarker {
        offset: u32,
        function: FunId,
        source_order: u32,
    },
    PropertyAccessor {
        offset: u32,
        function: Option<FunId>,
        property: PropertyAccessorDeclaration,
        setter: bool,
    },
}

#[derive(Clone, Copy)]
enum PropertyAccessorDeclaration {
    Member(usize),
    MemberExtension(usize),
    CompanionBlock(u32),
}

fn report_platform_declaration_clashes(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    diags: &mut DiagSink,
) -> bool {
    let mut clashes = Vec::new();
    for (class_id, class) in ir.classes.iter().enumerate() {
        let mut groups: crate::fx_hash::HashMap<(String, String), Vec<MemberMethodDeclaration>> =
            crate::fx_hash::HashMap::default();
        for &method in &class.methods {
            let Some(function) = ir.functions.get(method as usize) else {
                continue;
            };
            let declaration = if let Some(&offset) = ir.fn_signature_offsets.get(&method) {
                MemberMethodDeclaration::Source {
                    offset,
                    function: method,
                }
            } else if ir.is_data_class_member(class.fq_name_id(), method) {
                let Some(offset) = ir
                    .class_diagnostic_span(class_id as u32)
                    .map(|span| span.lo)
                else {
                    continue;
                };
                MemberMethodDeclaration::GeneratedDataMember {
                    offset,
                    function: method,
                }
            } else {
                continue;
            };
            let descriptor = crate::jvm::ir_emit::function_descriptor(ir, override_results, method);
            groups
                .entry((function.name.clone(), descriptor))
                .or_default()
                .push(declaration);
        }
        for (property_index, property) in class.properties.iter().enumerate() {
            for (setter, function, span, side) in [
                (
                    false,
                    property.getter,
                    property.getter_diagnostic_span,
                    crate::jvm::ir_emit::PropertyAccessorSide::Getter,
                ),
                (
                    true,
                    property.setter,
                    property.setter_diagnostic_span,
                    crate::jvm::ir_emit::PropertyAccessorSide::Setter,
                ),
            ] {
                let Some(offset) = span.or(property.diagnostic_span).map(|span| span.lo) else {
                    continue;
                };
                let signature = if let Some(function) = function {
                    if ir.fn_signature_offsets.contains_key(&function) {
                        continue;
                    }
                    let declaration = &ir.functions[function as usize];
                    Some((
                        declaration.name.clone(),
                        crate::jvm::ir_emit::function_descriptor(ir, override_results, function),
                    ))
                } else {
                    crate::jvm::ir_emit::declared_property_accessor_signature(
                        ir,
                        class,
                        property,
                        side,
                        override_results,
                    )
                    .map(|signature| (signature.name, signature.descriptor))
                };
                let Some(signature) = signature else {
                    continue;
                };
                groups.entry(signature).or_default().push(
                    MemberMethodDeclaration::PropertyAccessor {
                        offset,
                        function,
                        property: PropertyAccessorDeclaration::Member(property_index),
                        setter,
                    },
                );
            }
        }
        if let Some(properties) = ir.member_ext_props.get(&class.fq_name) {
            for (property_index, property) in properties.iter().enumerate() {
                for (setter, function, span) in [
                    (
                        false,
                        Some(property.getter),
                        property.getter_diagnostic_span,
                    ),
                    (true, property.setter, property.setter_diagnostic_span),
                ] {
                    let Some(function) = function else { continue };
                    if ir.fn_signature_offsets.contains_key(&function) {
                        continue;
                    }
                    let Some(offset) = span.or(property.diagnostic_span).map(|span| span.lo) else {
                        continue;
                    };
                    let declaration = &ir.functions[function as usize];
                    let signature = (
                        declaration.name.clone(),
                        crate::jvm::ir_emit::function_descriptor(ir, override_results, function),
                    );
                    groups.entry(signature).or_default().push(
                        MemberMethodDeclaration::PropertyAccessor {
                            offset,
                            function: Some(function),
                            property: PropertyAccessorDeclaration::MemberExtension(property_index),
                            setter,
                        },
                    );
                }
            }
        }
        for property in ir.companion_blocks.properties_of(class.fq_name_id()) {
            for (setter, function, span) in [
                (false, property.getter, property.getter_diagnostic_span),
                (true, property.setter, property.setter_diagnostic_span),
            ] {
                let Some(function) = function else {
                    continue;
                };
                if ir.fn_signature_offsets.contains_key(&function) {
                    continue;
                }
                let Some(offset) = span.or(property.diagnostic_span).map(|span| span.lo) else {
                    continue;
                };
                let declaration = &ir.functions[function as usize];
                let signature = (
                    declaration.name.clone(),
                    crate::jvm::ir_emit::function_descriptor(ir, override_results, function),
                );
                groups.entry(signature).or_default().push(
                    MemberMethodDeclaration::PropertyAccessor {
                        offset,
                        function: Some(function),
                        property: PropertyAccessorDeclaration::CompanionBlock(
                            property.source_order,
                        ),
                        setter,
                    },
                );
            }
        }
        for (&(owner, source_order), &marker) in &ir.property_annotation_markers {
            if owner != class.fq_name_id() {
                continue;
            }
            let class_offset = ir
                .class_diagnostic_span(class_id as u32)
                .expect("a source class with an annotation marker retains its diagnostic span")
                .lo;
            let function = &ir.functions[marker as usize];
            let descriptor = crate::jvm::ir_emit::function_descriptor(ir, override_results, marker);
            groups
                .entry((function.name.clone(), descriptor))
                .or_default()
                .push(MemberMethodDeclaration::PropertyAnnotationMarker {
                    offset: class_offset,
                    function: marker,
                    source_order,
                });
        }
        for ((name, descriptor), declarations) in groups {
            if declarations.len() < 2 {
                continue;
            }
            let includes_marker = declarations.iter().any(|declaration| {
                matches!(
                    declaration,
                    MemberMethodDeclaration::PropertyAnnotationMarker { .. }
                )
            });
            let includes_generated_data_member = declarations.iter().any(|declaration| {
                matches!(
                    declaration,
                    MemberMethodDeclaration::GeneratedDataMember { .. }
                )
            });
            let message = if includes_marker || includes_generated_data_member {
                let owner = class.fq_name.render().replace(['/', '$'], ".");
                let mut listed = declarations.clone();
                listed.sort_by_key(|declaration| match declaration {
                    MemberMethodDeclaration::GeneratedDataMember { .. } => 0,
                    MemberMethodDeclaration::PropertyAccessor {
                        property: PropertyAccessorDeclaration::MemberExtension(_),
                        ..
                    } => 2,
                    MemberMethodDeclaration::PropertyAccessor { .. } => 0,
                    MemberMethodDeclaration::PropertyAnnotationMarker { .. } => 1,
                    MemberMethodDeclaration::Source { .. } => 2,
                });
                let listed = listed
                    .into_iter()
                    .map(|declaration| {
                        render_member_method_declaration(ir, class, declaration, &name, &owner)
                    })
                    .collect::<Vec<_>>();
                clash_message(&name, &descriptor, &listed)
            } else {
                format!(
                    "platform declaration clash: The following declarations have the same JVM signature ({name}{descriptor}):"
                )
            };
            for declaration in declarations {
                let offset = match declaration {
                    MemberMethodDeclaration::Source { offset, .. }
                    | MemberMethodDeclaration::GeneratedDataMember { offset, .. }
                    | MemberMethodDeclaration::PropertyAnnotationMarker { offset, .. }
                    | MemberMethodDeclaration::PropertyAccessor { offset, .. } => offset,
                };
                clashes.push((offset, message.clone()));
            }
        }
    }
    clashes.sort_by_key(|(offset, _)| *offset);
    let found = !clashes.is_empty();
    for (offset, message) in clashes {
        diags.error(Span::new(offset, offset), message);
    }
    found
}

fn render_member_method_declaration(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    declaration: MemberMethodDeclaration,
    name: &str,
    owner: &str,
) -> String {
    let (function, parameters) = match declaration {
        MemberMethodDeclaration::Source { function, .. } => {
            return render_source_member_method(ir, function, name, owner);
        }
        MemberMethodDeclaration::GeneratedDataMember { function, .. } => {
            return render_source_member_method(ir, function, name, owner);
        }
        MemberMethodDeclaration::PropertyAccessor {
            function,
            property,
            setter,
            ..
        } => {
            return match property {
                PropertyAccessorDeclaration::Member(property) => {
                    render_property_accessor_declaration(
                        ir,
                        &class.properties[property],
                        function,
                        setter,
                        owner,
                    )
                }
                PropertyAccessorDeclaration::MemberExtension(property) => {
                    let property = &ir.member_ext_props[&class.fq_name][property];
                    let callable = if setter {
                        format!("<set-{}>", property.name)
                    } else {
                        format!("<get-{}>", property.name)
                    };
                    render_source_member_method(
                        ir,
                        function.expect("a source member-extension accessor has a function"),
                        &callable,
                        owner,
                    )
                }
                PropertyAccessorDeclaration::CompanionBlock(source_order) => {
                    let property = ir
                        .companion_blocks
                        .properties_of(class.fq_name_id())
                        .find(|property| property.source_order == source_order)
                        .expect("a recorded companion-block accessor retains its property");
                    let callable = if setter {
                        format!("<set-{}>", property.name)
                    } else {
                        format!("<get-{}>", property.name)
                    };
                    render_source_member_method(
                        ir,
                        function.expect("a source companion-block accessor has a function"),
                        &callable,
                        owner,
                    )
                }
            };
        }
        MemberMethodDeclaration::PropertyAnnotationMarker {
            function,
            source_order,
            ..
        } => (
            function,
            property_marker_semantic_parameters(ir, class, source_order),
        ),
    };
    let identities = ir
        .function_parameter_identities(function)
        .unwrap_or_default();
    assert_eq!(
        identities.len(),
        parameters.len(),
        "a colliding method retains one identity per semantic parameter"
    );
    let physical = &ir.functions[function as usize].params;
    let marker_names =
        crate::jvm::parameter_names::property_annotation_marker_locals(ir, function, physical)
            .expect("a property annotation marker retains its parameter identities");
    let mut rendered = format!("fun {}(", rendered_callable_name(name));
    for (index, ty) in parameters.into_iter().enumerate() {
        if index > 0 {
            rendered.push_str(", ");
        }
        let parameter = marker_names[index]
            .as_deref()
            .expect("a marker parameter has a diagnostic spelling");
        rendered.push_str(&rendered_callable_name(parameter));
        rendered.push_str(": ");
        rendered.push_str(
            &source_type_name(ty).expect("a reported member clash has determined source types"),
        );
    }
    rendered.push_str("): ");
    let ret = ir
        .vc_declared_sigs
        .get(&function)
        .map(|(_, _, ret)| *ret)
        .unwrap_or(ir.functions[function as usize].ret);
    rendered.push_str(
        &source_type_name(ret).expect("a reported member clash has a determined return type"),
    );
    rendered.push_str(" defined in ");
    rendered.push_str(owner);
    rendered
}

fn render_property_accessor_declaration(
    ir: &IrFile,
    property: &crate::ir::IrProperty,
    function: Option<FunId>,
    setter: bool,
    owner: &str,
) -> String {
    let callable = if setter {
        format!("<set-{}>", property.name)
    } else {
        format!("<get-{}>", property.name)
    };
    if let Some(function) = function {
        return render_source_member_method(ir, function, &callable, owner);
    }
    let mut parameters = property
        .context_params
        .iter()
        .map(|(name, kind, ty)| {
            let name = match kind {
                crate::types::ContextParameterKind::Named => name.as_str(),
                crate::types::ContextParameterKind::Anonymous => "_",
                crate::types::ContextParameterKind::LegacyReceiver => "_",
                crate::types::ContextParameterKind::None => {
                    panic!("a property context parameter retains its semantic role")
                }
            };
            format!(
                "{}: {}",
                rendered_callable_name(name),
                source_type_name(*ty)
                    .expect("a reported property-accessor clash has determined source types")
            )
        })
        .collect::<Vec<_>>();
    if setter {
        parameters.push(format!(
            "{}: {}",
            rendered_callable_name("<set-?>"),
            source_type_name(property.ty)
                .expect("a reported property-accessor clash has a determined property type")
        ));
    }
    let ret = if setter {
        crate::types::Ty::Unit
    } else {
        property.ty
    };
    format!(
        "fun {}({}): {} defined in {owner}",
        rendered_callable_name(&callable),
        parameters.join(", "),
        source_type_name(ret)
            .expect("a reported property-accessor clash has a determined return type")
    )
}

fn render_source_member_method(ir: &IrFile, function: FunId, name: &str, owner: &str) -> String {
    let parameters = semantic_function_parameters(ir, function);
    let defaults = ir.declared_param_defaults(function);
    let identities = ir
        .function_parameter_identities(function)
        .unwrap_or_default();
    assert_eq!(
        identities.len(),
        parameters.len(),
        "a colliding source method retains one identity per semantic parameter"
    );
    let mut contexts = Vec::new();
    let mut receiver = None;
    let mut values = Vec::new();
    let mut value_index = 0;
    for (identity, &ty) in identities.iter().zip(parameters) {
        let ty = source_type_name(ty).expect("a reported member clash has determined source types");
        match identity.role {
            crate::ir::IrParameterRole::ContextValue => {
                let name = identity
                    .source_name
                    .as_deref()
                    .expect("a named context parameter retains its source spelling");
                contexts.push(format!("{}: {ty}", rendered_callable_name(name)));
            }
            crate::ir::IrParameterRole::AnonymousContextParameter { .. } => {
                contexts.push(format!("_: {ty}"));
            }
            crate::ir::IrParameterRole::ContextReceiver { .. } => contexts.push(ty),
            crate::ir::IrParameterRole::ExtensionReceiver => {
                assert!(
                    receiver.replace(ty).is_none(),
                    "a function has one extension receiver"
                );
            }
            crate::ir::IrParameterRole::Value => {
                let name = identity
                    .source_name
                    .as_deref()
                    .expect("a source value parameter retains its source spelling");
                let mut value = format!("{}: {ty}", rendered_callable_name(name));
                if defaults
                    .and_then(|defaults| defaults.get(value_index))
                    .is_some_and(Option::is_some)
                {
                    value.push_str(" = ...");
                }
                values.push(value);
                value_index += 1;
            }
            role => panic!("a source member clash contains non-source parameter role {role:?}"),
        }
    }
    let mut rendered = String::new();
    if !contexts.is_empty() {
        rendered.push_str("context(");
        rendered.push_str(&contexts.join(", "));
        rendered.push_str(") ");
    }
    rendered.push_str("fun ");
    if let Some(receiver) = receiver {
        rendered.push_str(&receiver);
        rendered.push('.');
    }
    rendered.push_str(&rendered_callable_name(name));
    rendered.push('(');
    rendered.push_str(&values.join(", "));
    rendered.push_str("): ");
    let ret = ir
        .vc_declared_sigs
        .get(&function)
        .map(|(_, _, ret)| *ret)
        .unwrap_or(ir.functions[function as usize].ret);
    rendered.push_str(
        &source_type_name(ret).expect("a reported member clash has a determined return type"),
    );
    rendered.push_str(" defined in ");
    rendered.push_str(owner);
    rendered
}

fn semantic_function_parameters(ir: &IrFile, function: FunId) -> &[crate::types::Ty] {
    ir.vc_declared_sigs
        .get(&function)
        .map(|(_, parameters, _)| parameters.as_slice())
        .or_else(|| {
            ir.signatures
                .get(&function)
                .map(|signature| signature.params.as_slice())
        })
        .unwrap_or(&ir.functions[function as usize].params)
}

fn property_marker_semantic_parameters(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    source_order: u32,
) -> Vec<crate::types::Ty> {
    if let Some(property) = class
        .properties
        .iter()
        .find(|property| property.source_order == source_order)
    {
        return property.getter.map_or_else(
            || {
                property
                    .context_params
                    .iter()
                    .map(|(_, _, ty)| *ty)
                    .collect()
            },
            |getter| semantic_function_parameters(ir, getter).to_vec(),
        );
    }
    if let Some(property) = ir
        .member_ext_props
        .get(&class.fq_name_id())
        .and_then(|properties| {
            properties
                .iter()
                .find(|property| property.source_order == source_order)
        })
    {
        return semantic_function_parameters(ir, property.getter).to_vec();
    }
    if let Some(property) = ir
        .companion_blocks
        .properties_of(class.fq_name_id())
        .find(|property| property.source_order == source_order)
    {
        return property
            .getter
            .map(|getter| semantic_function_parameters(ir, getter).to_vec())
            .unwrap_or_default();
    }
    Vec::new()
}

fn reject_duplicate_constructors(class: &crate::ir::IrClass) -> Result<(), String> {
    let mut descriptors = crate::fx_hash::HashSet::default();
    if class.has_primary_ctor {
        descriptors.insert(crate::jvm::names::method_descriptor(
            &crate::jvm::ir_emit::class_ctor_jvm_tys(class),
            crate::types::Ty::Unit,
        ));
    }
    for constructor in &class.secondary_ctors {
        let parameters = crate::jvm::ir_emit::jvm_tys(&constructor.prefix_params)
            .into_iter()
            .chain(crate::jvm::ir_emit::jvm_tys(&constructor.params))
            .collect::<Vec<_>>();
        let descriptor = crate::jvm::names::method_descriptor(&parameters, crate::types::Ty::Unit);
        if !descriptors.insert(descriptor.clone()) {
            return Err(format!(
                "platform declaration clash: '{}' contains duplicate JVM constructor <init>{descriptor}",
                class.fq_name
            ));
        }
    }
    Ok(())
}

fn reject_uninventoried_duplicate_method(
    ir: &IrFile,
    class_id: crate::ir::ClassId,
    class: &crate::ir::IrClass,
    accessor: FunId,
) -> Result<(), String> {
    let Some(accessor_function) = ir.functions.get(accessor as usize) else {
        return Err("internal error: member extension accessor has no JVM function".to_string());
    };
    let descriptor =
        crate::jvm::names::method_descriptor(&accessor_function.params, accessor_function.ret);
    let accessor_is_inventoried = member_method_is_inventoried(ir, class_id, class, accessor);
    if class.methods.iter().copied().any(|candidate| {
        candidate != accessor
            // Two inventoried declarations were compared by the exact inventory above. When this
            // accessor has no semantic provenance, however, it was absent from that inventory and
            // must still be compared with every physical method, including source declarations.
            && (!accessor_is_inventoried
                || !member_method_is_inventoried(ir, class_id, class, candidate))
            && ir
                .functions
                .get(candidate as usize)
                .is_some_and(|function| {
                    function.name == accessor_function.name
                        && crate::jvm::names::method_descriptor(&function.params, function.ret)
                            == descriptor
                })
    }) {
        return Err(format!(
            "platform declaration clash: '{}' contains duplicate JVM method {}{}",
            class.fq_name, accessor_function.name, descriptor
        ));
    }
    Ok(())
}

/// Whether the exact declaration-clash inventory above already owns this method identity.
///
/// Compiler passes may append representation-only helpers to `class.methods` without a source
/// signature. Those still need the conservative duplicate-method fallback. Source functions,
/// property accessors, annotation markers, and generated data-class declarations all have a
/// semantic declaration shape and must not also produce that fallback's generic diagnostic.
fn member_method_is_inventoried(
    ir: &IrFile,
    class_id: crate::ir::ClassId,
    class: &crate::ir::IrClass,
    function: FunId,
) -> bool {
    ir.fn_signature_offsets.contains_key(&function)
        || (ir.is_data_class_member(class.fq_name_id(), function)
            && ir.class_diagnostic_span(class_id).is_some())
        || class.properties.iter().any(|property| {
            (property.getter == Some(function)
                && property
                    .getter_diagnostic_span
                    .or(property.diagnostic_span)
                    .is_some())
                || (property.setter == Some(function)
                    && property
                        .setter_diagnostic_span
                        .or(property.diagnostic_span)
                        .is_some())
        })
        || ir
            .member_ext_props
            .get(&class.fq_name)
            .is_some_and(|properties| {
                properties.iter().any(|property| {
                    (property.getter == function
                        && property
                            .getter_diagnostic_span
                            .or(property.diagnostic_span)
                            .is_some())
                        || (property.setter == Some(function)
                            && property
                                .setter_diagnostic_span
                                .or(property.diagnostic_span)
                                .is_some())
                })
            })
        || ir
            .companion_blocks
            .properties_of(class.fq_name_id())
            .any(|property| {
                (property.getter == Some(function)
                    && property
                        .getter_diagnostic_span
                        .or(property.diagnostic_span)
                        .is_some())
                    || (property.setter == Some(function)
                        && property
                            .setter_diagnostic_span
                            .or(property.diagnostic_span)
                            .is_some())
            })
        || ir
            .property_annotation_markers
            .iter()
            .any(|(&(owner, _), &marker)| owner == class.fq_name_id() && marker == function)
}

/// Source package functions and generated property-annotation markers whose representation passes
/// selected the same facade method.
///
/// Different Kotlin shapes (`fun <T> id(): Int` and `fun id(): Int`, or `List<T>` and
/// `List<String>`) stay distinct in the resolver. They collide only once the physical name and
/// descriptor are known, and only inside one file facade.
///
/// Both variants carry `(span, message)` rows: `Ok` the clashes to report, `Err` the failure to
/// report instead.
type ClashRows = Vec<(crate::diag::Span, String)>;

#[derive(Clone, Copy)]
enum PackageMethodDeclaration {
    Function(usize),
    PropertyAnnotationMarker(FunId),
}

fn package_function_signature_clashes(ir: &IrFile) -> Result<ClashRows, ClashRows> {
    let mut groups: Vec<((String, String), Vec<PackageMethodDeclaration>)> = Vec::new();
    for (index, declaration) in ir.package_functions.iter().enumerate() {
        if declaration.companion {
            continue;
        }
        let Some(function) = ir.functions.get(declaration.function as usize) else {
            return Err(vec![(
                declaration.signature_span,
                "internal error: package function platform-clash validation has no IR function"
                    .to_string(),
            )]);
        };
        if let Err(kind) = determined_function_signature(function) {
            return Err(vec![(
                declaration.signature_span,
                undetermined_type_message(kind),
            )]);
        }
        let signature = (
            function.name.clone(),
            crate::jvm::names::method_descriptor(&function.params, function.ret),
        );
        if let Some((_, members)) = groups.iter_mut().find(|(key, _)| *key == signature) {
            members.push(PackageMethodDeclaration::Function(index));
        } else {
            groups.push((signature, vec![PackageMethodDeclaration::Function(index)]));
        }
    }
    for property in &ir.package_properties {
        let Some(marker) = ir.jvm_package_property_annotation_marker(property.property) else {
            continue;
        };
        let Some(function) = ir.functions.get(marker as usize) else {
            return Err(vec![(
                Span::new(0, 0),
                "internal error: package property annotation marker has no IR function".to_string(),
            )]);
        };
        if let Err(kind) = determined_function_signature(function) {
            return Err(vec![(Span::new(0, 0), undetermined_type_message(kind))]);
        }
        let signature = (
            function.name.clone(),
            crate::jvm::names::method_descriptor(&function.params, function.ret),
        );
        let declaration = PackageMethodDeclaration::PropertyAnnotationMarker(marker);
        if let Some((_, members)) = groups.iter_mut().find(|(key, _)| *key == signature) {
            members.push(declaration);
        } else {
            groups.push((signature, vec![declaration]));
        }
    }
    let mut clashes = Vec::new();
    for ((name, descriptor), members) in groups {
        if members.len() < 2 {
            continue;
        }
        let mut rendered = Vec::with_capacity(members.len());
        for &declaration in &members {
            let key = package_declaration_order(ir, declaration).map_err(|kind| {
                vec![(
                    package_declaration_span(ir, declaration),
                    undetermined_type_message(kind),
                )]
            })?;
            rendered.push((declaration, key));
        }
        rendered.sort_by(|(_, left), (_, right)| left.cmp(right));
        let listed = rendered
            .iter()
            .map(|(declaration, _)| render_package_declaration(ir, *declaration))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|kind| {
                vec![(
                    package_declaration_span(ir, rendered[0].0),
                    undetermined_type_message(kind),
                )]
            })?;
        let message = clash_message(&name, &descriptor, &listed);
        let mut located = members
            .into_iter()
            .map(|declaration| (package_declaration_span(ir, declaration), message.clone()))
            .collect::<Vec<_>>();
        located.sort_by_key(|(span, _)| span.lo);
        clashes.extend(located);
    }
    clashes.sort_by_key(|(span, _)| span.lo);
    Ok(clashes)
}

fn package_declaration_span(ir: &IrFile, declaration: PackageMethodDeclaration) -> Span {
    match declaration {
        PackageMethodDeclaration::Function(index) => ir.package_functions[index].signature_span,
        // kotlinc locates a generated package-property annotation marker at the file header.
        PackageMethodDeclaration::PropertyAnnotationMarker(_) => Span::new(0, 0),
    }
}

fn package_declaration_order(
    ir: &IrFile,
    declaration: PackageMethodDeclaration,
) -> Result<(String, String, Vec<String>, usize), UndeterminedDiagnosticType> {
    match declaration {
        PackageMethodDeclaration::Function(index) => {
            declaration_order(&ir.package_functions[index])
        }
        PackageMethodDeclaration::PropertyAnnotationMarker(function) => {
            let function = &ir.functions[function as usize];
            Ok((function.name.clone(), String::new(), Vec::new(), 0))
        }
    }
}

fn render_package_declaration(
    ir: &IrFile,
    declaration: PackageMethodDeclaration,
) -> Result<String, UndeterminedDiagnosticType> {
    match declaration {
        PackageMethodDeclaration::Function(index) => {
            render_declaration(&ir.package_functions[index], ir.package.as_deref())
        }
        PackageMethodDeclaration::PropertyAnnotationMarker(function) => {
            let function = &ir.functions[function as usize];
            let mut rendered = format!(
                "fun {}(): Unit defined in ",
                rendered_callable_name(&function.name)
            );
            match ir.package.as_deref() {
                Some(package) if !package.is_empty() => rendered.push_str(package),
                _ => rendered.push_str("root package"),
            }
            Ok(rendered)
        }
    }
}

fn clash_message(name: &str, descriptor: &str, declarations: &[String]) -> String {
    let mut message = format!(
        "platform declaration clash: The following declarations have the same JVM signature ({name}{descriptor}):"
    );
    for declaration in declarations {
        message.push('\n');
        message.push_str("    ");
        message.push_str(declaration);
    }
    message
}

fn value_parameters(
    function: &crate::ir::IrPackageFunction,
) -> Result<&[(String, crate::types::Ty)], UndeterminedDiagnosticType> {
    function
        .params
        .get(function.context_count..)
        .ok_or(UndeterminedDiagnosticType::InvalidContextParameterCount)
}

fn render_declaration(
    function: &crate::ir::IrPackageFunction,
    package: Option<&str>,
) -> Result<String, UndeterminedDiagnosticType> {
    let mut rendered = String::from("fun ");
    if !function.type_params.is_empty() {
        rendered.push('<');
        for (index, parameter) in function.type_params.iter().enumerate() {
            if index > 0 {
                rendered.push_str(", ");
            }
            rendered.push_str(&parameter.name);
            let bounds = parameter
                .bounds
                .iter()
                .copied()
                .filter(|bound| !is_default_bound(*bound))
                .collect::<Vec<_>>();
            if !bounds.is_empty() {
                rendered.push_str(" : ");
                for (bound_index, bound) in bounds.iter().enumerate() {
                    if bound_index > 0 {
                        rendered.push_str(" & ");
                    }
                    rendered.push_str(&source_type_name(*bound)?);
                }
            }
        }
        rendered.push_str("> ");
    }
    if let Some(receiver) = function.receiver {
        rendered.push_str(&source_type_name(receiver)?);
        rendered.push('.');
    }
    rendered.push_str(&rendered_callable_name(&function.name));
    rendered.push('(');
    for (index, (name, ty)) in value_parameters(function)?.iter().enumerate() {
        if index > 0 {
            rendered.push_str(", ");
        }
        rendered.push_str(name);
        rendered.push_str(": ");
        rendered.push_str(&source_type_name(*ty)?);
    }
    rendered.push_str("): ");
    rendered.push_str(&source_type_name(function.ret)?);
    rendered.push_str(" defined in ");
    match package {
        Some(package) if !package.is_empty() => rendered.push_str(package),
        _ => rendered.push_str("root package"),
    }
    Ok(rendered)
}

fn rendered_callable_name(name: &str) -> String {
    let mut characters = name.chars();
    let ordinary = characters
        .next()
        .is_some_and(|first| first == '_' || first.is_alphabetic())
        && characters.all(|character| character == '_' || character.is_alphanumeric());
    if ordinary {
        name.to_string()
    } else {
        format!("`{name}`")
    }
}

/// kotlinc's `MemberComparator` order: name, then rendered receiver and value-parameter types
/// (classifiers qualified), then type-parameter count.
fn declaration_order(
    function: &crate::ir::IrPackageFunction,
) -> Result<(String, String, Vec<String>, usize), UndeterminedDiagnosticType> {
    Ok((
        function.name.clone(),
        function
            .receiver
            .map(qualified_type_name)
            .transpose()?
            .unwrap_or_default(),
        value_parameters(function)?
            .iter()
            .map(|(_, ty)| qualified_type_name(*ty))
            .collect::<Result<Vec<_>, _>>()?,
        function.type_params.len(),
    ))
}

fn is_default_bound(bound: crate::types::Ty) -> bool {
    let inner = match bound {
        crate::types::Ty::Nullable(inner) => *inner,
        other => other,
    };
    matches!(inner, crate::types::Ty::Obj(name, arguments)
        if arguments.is_empty() && name == crate::types::wk::any())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UndeterminedDiagnosticType {
    Error,
    Pending,
    InvalidContextParameterCount,
}

fn undetermined_type_message(kind: UndeterminedDiagnosticType) -> String {
    match kind {
        UndeterminedDiagnosticType::Error => {
            "internal error: package function platform-clash diagnostic contains an error type"
                .to_string()
        }
        UndeterminedDiagnosticType::Pending => {
            "internal error: package function platform-clash diagnostic contains a pending type"
                .to_string()
        }
        UndeterminedDiagnosticType::InvalidContextParameterCount => {
            "internal error: package function platform-clash diagnostic has an invalid context-parameter count"
                .to_string()
        }
    }
}

fn determined_function_signature(
    function: &crate::ir::IrFunction,
) -> Result<(), UndeterminedDiagnosticType> {
    for &parameter in &function.params {
        ensure_determined_type(parameter)?;
    }
    ensure_determined_type(function.ret)?;
    Ok(())
}

fn source_type_name(ty: crate::types::Ty) -> Result<String, UndeterminedDiagnosticType> {
    ensure_determined_type(ty)?;
    Ok(ty.source_name())
}

fn ensure_determined_type(ty: crate::types::Ty) -> Result<(), UndeterminedDiagnosticType> {
    use crate::types::Ty;
    match ty {
        Ty::Error => Err(UndeterminedDiagnosticType::Error),
        Ty::Pending => Err(UndeterminedDiagnosticType::Pending),
        Ty::Obj(_, arguments) => {
            for &argument in arguments {
                ensure_determined_type(argument)?;
            }
            Ok(())
        }
        Ty::Fun(signature) => {
            for &parameter in &signature.params {
                ensure_determined_type(parameter)?;
            }
            ensure_determined_type(signature.ret)
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner)
        | Ty::StarProjection(inner)
        | Ty::TyParam(_, inner) => ensure_determined_type(*inner),
        Ty::Intersection(parts) => {
            for &part in parts {
                ensure_determined_type(part)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn qualified_type_name(ty: crate::types::Ty) -> Result<String, UndeterminedDiagnosticType> {
    use crate::types::Ty;
    Ok(match ty {
        Ty::Int => "kotlin.Int".to_string(),
        Ty::Byte => "kotlin.Byte".to_string(),
        Ty::Short => "kotlin.Short".to_string(),
        Ty::Long => "kotlin.Long".to_string(),
        Ty::Float => "kotlin.Float".to_string(),
        Ty::Double => "kotlin.Double".to_string(),
        Ty::Boolean => "kotlin.Boolean".to_string(),
        Ty::Char => "kotlin.Char".to_string(),
        Ty::UByte => "kotlin.UByte".to_string(),
        Ty::UShort => "kotlin.UShort".to_string(),
        Ty::UInt => "kotlin.UInt".to_string(),
        Ty::ULong => "kotlin.ULong".to_string(),
        Ty::String => "kotlin.String".to_string(),
        Ty::Unit => "kotlin.Unit".to_string(),
        Ty::Nothing => "kotlin.Nothing".to_string(),
        Ty::Null => "null".to_string(),
        Ty::Obj(name, arguments) => {
            let base = name.render().replace(['/', '$'], ".");
            if arguments.is_empty() {
                base
            } else {
                let arguments = arguments
                    .iter()
                    .map(|argument| qualified_type_name(*argument))
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ");
                format!("{base}<{arguments}>")
            }
        }
        Ty::Nullable(inner) => {
            let rendered = qualified_type_name(*inner)?;
            if matches!(*inner, Ty::Fun(_)) {
                format!("({rendered})?")
            } else {
                format!("{rendered}?")
            }
        }
        Ty::PlatformNullable(inner) => format!("{}!", qualified_type_name(*inner)?),
        Ty::DefinitelyNotNull(inner) => {
            format!("{} & Any", qualified_type_name(*inner)?)
        }
        Ty::Intersection(parts) => parts
            .iter()
            .map(|part| qualified_type_name(*part))
            .collect::<Result<Vec<_>, _>>()?
            .join(" & "),
        Ty::TyParam(name, _) => crate::types::type_parameter_source_name(name).to_string(),
        Ty::Fun(signature) => {
            let parameters = signature
                .params
                .iter()
                .map(|parameter| qualified_type_name(*parameter))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            let suspend = if signature.suspend { "suspend " } else { "" };
            format!(
                "{suspend}({parameters}) -> {}",
                qualified_type_name(signature.ret)?
            )
        }
        Ty::InProjection(inner) => format!("in {}", qualified_type_name(*inner)?),
        Ty::OutProjection(inner) => format!("out {}", qualified_type_name(*inner)?),
        Ty::StarProjection(_) => "*".to_string(),
        Ty::Error => return Err(UndeterminedDiagnosticType::Error),
        Ty::Pending => return Err(UndeterminedDiagnosticType::Pending),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member_function(owner: crate::types::TypeName, name: &str) -> crate::ir::IrFunction {
        crate::ir::IrFunction {
            name: name.to_string(),
            params: Vec::new(),
            ret: crate::types::Ty::Int,
            body: None,
            is_static: false,
            dispatch_receiver: Some(owner),
            param_checks: Vec::new(),
        }
    }

    #[test]
    fn uninventoried_member_extension_accessor_still_clashes_with_source_method() {
        let mut ir = IrFile::default();
        let owner = crate::types::type_name("fixtures/Owner");
        let source = ir.add_fun(member_function(owner, "getValue"));
        let generated_accessor = ir.add_fun(member_function(owner, "getValue"));
        ir.fn_signature_offsets.insert(source, 11);

        let mut class = crate::ir::IrClass::synthetic(owner);
        class.methods = vec![source, generated_accessor];
        let class_id = ir.add_class(class);
        ir.member_ext_props
            .entry(owner)
            .or_default()
            .push(crate::ir::MemberExtProp {
                name: "value".to_string(),
                diagnostic_span: None,
                getter_diagnostic_span: None,
                setter_diagnostic_span: None,
                source_order: u32::MAX,
                receiver: crate::types::Ty::Int,
                ty: crate::types::Ty::Int,
                is_var: false,
                is_abstract: false,
                modifiers: crate::ir::IrPropertyModifiers::default(),
                delegate_field: None,
                getter: generated_accessor,
                setter: None,
                visibility: crate::types::Visibility::Public,
                type_params: Vec::new(),
            });

        assert!(member_method_is_inventoried(
            &ir,
            class_id,
            &ir.classes[class_id as usize],
            source,
        ));
        assert!(!member_method_is_inventoried(
            &ir,
            class_id,
            &ir.classes[class_id as usize],
            generated_accessor,
        ));
        assert_eq!(
            reject_uninventoried_duplicate_method(
                &ir,
                class_id,
                &ir.classes[class_id as usize],
                generated_accessor,
            ),
            Err(
                "platform declaration clash: 'fixtures/Owner' contains duplicate JVM method getValue()I"
                    .to_string(),
            ),
        );
    }

    #[test]
    fn diagnostic_type_rendering_rejects_undetermined_types() {
        assert_eq!(
            qualified_type_name(crate::types::Ty::Error),
            Err(UndeterminedDiagnosticType::Error)
        );
        assert_eq!(
            qualified_type_name(crate::types::Ty::Pending),
            Err(UndeterminedDiagnosticType::Pending)
        );
        assert_eq!(
            qualified_type_name(crate::types::Ty::obj_args(
                "repo/Payload",
                &[crate::types::Ty::Error]
            )),
            Err(UndeterminedDiagnosticType::Error)
        );
        assert_eq!(
            undetermined_type_message(UndeterminedDiagnosticType::Error),
            "internal error: package function platform-clash diagnostic contains an error type"
        );
        assert_eq!(
            undetermined_type_message(UndeterminedDiagnosticType::Pending),
            "internal error: package function platform-clash diagnostic contains a pending type"
        );
    }

    #[test]
    fn diagnostic_type_rendering_preserves_definitely_non_null_identity() {
        let caller = crate::types::Ty::ty_param(
            "T",
            crate::types::Ty::nullable(crate::types::Ty::obj_name(crate::types::wk::any())),
        );
        let intersection = caller.contributed_through_nullable_formal();

        assert_eq!(qualified_type_name(intersection), Ok("T & Any".to_string()));
    }

    #[test]
    fn default_bound_uses_resolved_builtin_identity() {
        assert!(is_default_bound(crate::types::Ty::nullable(
            crate::types::Ty::obj_name(crate::types::wk::any())
        )));
        assert!(!is_default_bound(crate::types::Ty::obj("repo/Any")));
    }
}
