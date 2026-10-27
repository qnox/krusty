//! JVM declaration-signature validation after representation selection.
//!
//! Kotlin declarations can be distinct at the source level while their JVM realizations collide.
//! This pass runs only after representation passes have selected physical method names and types,
//! so FIR and common lowering never depend on JVM descriptors.

use crate::diag::{DiagSink, Span};
use crate::ir::{FunId, IrFile};
use crate::types::TyKind;

mod local_function_diagnostics;
mod package_method_diagnostics;
mod property_marker_diagnostics;

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
    for class in &ir.classes {
        if let Err(message) = reject_duplicate_constructors(ir, class) {
            diags.error(Span::new(0, 0), message);
            return Err(());
        }
    }
    let package_clashes = match package_method_diagnostics::report(ir, override_results) {
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
    let local_function_clashes = match local_function_diagnostics::report(ir, override_results) {
        Ok(clashes) => clashes,
        Err(errors) => {
            for (span, message) in errors {
                diags.error(span, message);
            }
            return Err(());
        }
    };
    let local_function_clash = !local_function_clashes.is_empty();
    for (span, message) in local_function_clashes {
        diags.error(span, message);
    }
    if platform_clash || package_clash || local_function_clash {
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
        let name = crate::jvm::companion_storage::field_name(class.fq_name, companion);
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
    /// The outer class's static bridge of `@JvmStatic` companion member `function`, reported at
    /// that member's declaration.
    CompanionBridge {
        offset: u32,
        function: FunId,
    },
    GeneratedDataMember {
        offset: u32,
        function: FunId,
    },
    GeneratedMethod {
        offset: u32,
    },
    InPlaceDelegate {
        offset: u32,
        function: FunId,
        source_order: u32,
    },
    DelegationForwarder {
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

impl MemberMethodDeclaration {
    /// The source offset this declaration's clash is reported at.
    fn offset(self) -> u32 {
        match self {
            Self::Source { offset, .. }
            | Self::CompanionBridge { offset, .. }
            | Self::GeneratedDataMember { offset, .. }
            | Self::GeneratedMethod { offset }
            | Self::InPlaceDelegate { offset, .. }
            | Self::DelegationForwarder { offset, .. }
            | Self::PropertyAnnotationMarker { offset, .. }
            | Self::PropertyAccessor { offset, .. } => offset,
        }
    }
}

#[derive(Clone, Copy)]
enum PropertyAccessorDeclaration {
    Member(usize),
    MemberExtension(usize),
    CompanionBlock(u32),
}

fn property_accessor_declaration(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    function: FunId,
) -> Option<(PropertyAccessorDeclaration, bool)> {
    for (index, property) in class.properties.iter().enumerate() {
        if property.getter == Some(function) {
            return Some((PropertyAccessorDeclaration::Member(index), false));
        }
        if property.setter == Some(function) {
            return Some((PropertyAccessorDeclaration::Member(index), true));
        }
    }
    if let Some(properties) = ir.member_ext_props.get(&class.fq_name) {
        for (index, property) in properties.iter().enumerate() {
            if property.getter == function {
                return Some((PropertyAccessorDeclaration::MemberExtension(index), false));
            }
            if property.setter == Some(function) {
                return Some((PropertyAccessorDeclaration::MemberExtension(index), true));
            }
        }
    }
    ir.companion_blocks
        .properties_of(class.fq_name_id())
        .find_map(|property| {
            if property.getter == Some(function) {
                Some((
                    PropertyAccessorDeclaration::CompanionBlock(property.source_order),
                    false,
                ))
            } else if property.setter == Some(function) {
                Some((
                    PropertyAccessorDeclaration::CompanionBlock(property.source_order),
                    true,
                ))
            } else {
                None
            }
        })
}

fn report_platform_declaration_clashes(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    diags: &mut DiagSink,
) -> bool {
    let mut clashes = Vec::new();
    let delegate_getters = ir
        .jvm_delegate_methods_with_getters()
        .collect::<crate::fx_hash::HashMap<_, _>>();
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
            } else if let Some(&getter) = delegate_getters.get(&method) {
                MemberMethodDeclaration::InPlaceDelegate {
                    offset: ir
                        .class_diagnostic_span(class_id as u32)
                        .map_or(0, |span| span.lo),
                    function: method,
                    source_order: ir.fn_source_order.get(&getter).copied().unwrap_or(u32::MAX),
                }
            } else if member_method_is_inventoried(ir, class_id as u32, class, method) {
                // A property accessor or annotation marker is inserted below with its semantic
                // declaration identity and exact source span.
                continue;
            } else if ir.local_function_spans.contains_key(&method) {
                // A lifted local function is reported at its own declaration by
                // `local_function_diagnostics`.
                continue;
            } else if ir.interface_delegation_forwarders.contains(&method) {
                let offset = ir
                    .class_diagnostic_span(class_id as u32)
                    .map_or(0, |span| span.lo);
                if let Some((property, setter)) = property_accessor_declaration(ir, class, method) {
                    MemberMethodDeclaration::PropertyAccessor {
                        offset,
                        function: Some(method),
                        property,
                        setter,
                    }
                } else {
                    MemberMethodDeclaration::DelegationForwarder {
                        offset,
                        function: method,
                    }
                }
            } else {
                // Uninventoried representation-only methods still occupy the same physical
                // namespace as implicit property accessors. Associate them with the owning class
                // for diagnostics; the group key already carries the descriptor selected by the
                // backend from their function identity.
                MemberMethodDeclaration::GeneratedMethod {
                    offset: ir
                        .class_diagnostic_span(class_id as u32)
                        .map_or(0, |span| span.lo),
                }
            };
            let descriptor = crate::jvm::ir_emit::function_descriptor(ir, override_results, method);
            groups
                .entry((function.name.clone(), descriptor))
                .or_default()
                .push(declaration);
        }
        for function in
            crate::jvm::ir_emit::jvm_static_members::companion_bridge_functions(ir, class)
        {
            let Some(&offset) = ir.fn_signature_offsets.get(&function) else {
                continue;
            };
            let descriptor =
                crate::jvm::ir_emit::function_descriptor(ir, override_results, function);
            groups
                .entry((ir.functions[function as usize].name.clone(), descriptor))
                .or_default()
                .push(MemberMethodDeclaration::CompanionBridge { offset, function });
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
        let mut groups = groups
            .into_iter()
            .filter(|(_, declarations)| declarations.len() >= 2)
            .collect::<Vec<_>>();
        groups.sort_by(|(left_signature, left), (right_signature, right)| {
            property_marker_diagnostics::member_method_group_source_order(ir, class, left)
                .cmp(
                    &property_marker_diagnostics::member_method_group_source_order(
                        ir, class, right,
                    ),
                )
                .then_with(|| left_signature.cmp(right_signature))
        });
        let mut reported_marker_header = false;
        let mut reported_delegate_header = false;
        for ((name, descriptor), declarations) in groups {
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
            let includes_generated_method = declarations.iter().any(|declaration| {
                matches!(declaration, MemberMethodDeclaration::GeneratedMethod { .. })
            });
            let includes_delegate = declarations.iter().any(|declaration| {
                matches!(declaration, MemberMethodDeclaration::InPlaceDelegate { .. })
            });
            let includes_delegation_forwarder =
                declarations.iter().any(|declaration| match declaration {
                    MemberMethodDeclaration::DelegationForwarder { .. } => true,
                    MemberMethodDeclaration::PropertyAccessor {
                        function: Some(function),
                        ..
                    } => ir.interface_delegation_forwarders.contains(function),
                    _ => false,
                });
            let includes_companion_bridge = declarations.iter().any(|declaration| {
                matches!(declaration, MemberMethodDeclaration::CompanionBridge { .. })
            });
            let declarations_are_renderable = declarations.iter().all(|&declaration| {
                member_method_declaration_is_renderable(ir, class, declaration)
            });
            let message = if !includes_generated_method
                && declarations_are_renderable
                && (includes_marker
                    || includes_generated_data_member
                    || includes_delegate
                    || includes_delegation_forwarder
                    || includes_companion_bridge)
            {
                let owner = class.fq_name.render().replace(['/', '$'], ".");
                let mut listed = declarations.clone();
                // kotlinc lists a companion bridge and the members it clashes with in source order.
                if includes_companion_bridge {
                    listed.sort_by_key(|declaration| declaration.offset());
                }
                let kind_order = |declaration: &MemberMethodDeclaration| match declaration {
                    MemberMethodDeclaration::GeneratedDataMember { .. } => 0,
                    MemberMethodDeclaration::GeneratedMethod { .. } => 0,
                    MemberMethodDeclaration::InPlaceDelegate { .. } => 0,
                    MemberMethodDeclaration::DelegationForwarder { .. } => 1,
                    MemberMethodDeclaration::PropertyAccessor {
                        property: PropertyAccessorDeclaration::MemberExtension(_),
                        ..
                    } => 2,
                    MemberMethodDeclaration::PropertyAccessor { .. } => 0,
                    MemberMethodDeclaration::PropertyAnnotationMarker { .. } => 1,
                    MemberMethodDeclaration::Source { .. }
                    | MemberMethodDeclaration::CompanionBridge { .. } => 2,
                };
                listed.sort_by_key(kind_order);
                let type_context = listed
                    .iter()
                    .flat_map(|declaration| {
                        property_marker_diagnostics::declaration_types(ir, class, *declaration)
                    })
                    .collect::<Vec<_>>();
                property_marker_diagnostics::order_marker_sources(ir, &mut listed);
                let listed = listed
                    .into_iter()
                    .map(|declaration| {
                        render_member_method_declaration(
                            ir,
                            class,
                            declaration,
                            &name,
                            &owner,
                            &type_context,
                        )
                    })
                    .collect::<Vec<_>>();
                clash_message(&name, &descriptor, &listed)
            } else {
                format!(
                    "platform declaration clash: The following declarations have the same JVM signature ({name}{descriptor}):"
                )
            };
            for declaration in declarations {
                // A class header carries one report for its delegates and one for its markers.
                match declaration {
                    MemberMethodDeclaration::InPlaceDelegate { .. } => {
                        if reported_delegate_header {
                            continue;
                        }
                        reported_delegate_header = true;
                    }
                    MemberMethodDeclaration::PropertyAnnotationMarker { .. } => {
                        if reported_marker_header {
                            continue;
                        }
                        reported_marker_header = true;
                    }
                    _ => {}
                }
                clashes.push((declaration.offset(), message.clone()));
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

fn member_method_declaration_is_renderable(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    declaration: MemberMethodDeclaration,
) -> bool {
    let function = match declaration {
        MemberMethodDeclaration::Source { function, .. }
        | MemberMethodDeclaration::CompanionBridge { function, .. }
        | MemberMethodDeclaration::GeneratedDataMember { function, .. }
        | MemberMethodDeclaration::DelegationForwarder { function, .. } => Some(function),
        MemberMethodDeclaration::InPlaceDelegate { function, .. } => {
            return determined_function_signature(&ir.functions[function as usize]).is_ok();
        }
        MemberMethodDeclaration::PropertyAccessor {
            function: Some(function),
            property: PropertyAccessorDeclaration::Member(_),
            ..
        } => Some(function),
        MemberMethodDeclaration::PropertyAccessor {
            function: None,
            property: PropertyAccessorDeclaration::Member(index),
            ..
        } => {
            let Some(property) = class.properties.get(index) else {
                return false;
            };
            return ensure_determined_type(property.ty).is_ok()
                && property
                    .context_params
                    .iter()
                    .all(|(_, _, ty)| ensure_determined_type(*ty).is_ok());
        }
        MemberMethodDeclaration::PropertyAccessor {
            function,
            property:
                PropertyAccessorDeclaration::MemberExtension(_)
                | PropertyAccessorDeclaration::CompanionBlock(_),
            ..
        } => {
            let Some(function) = function else {
                return false;
            };
            Some(function)
        }
        MemberMethodDeclaration::PropertyAnnotationMarker { .. } => return true,
        MemberMethodDeclaration::GeneratedMethod { .. } => return false,
    };
    let function = function.expect("renderable source declaration has a function identity");
    let parameters = semantic_function_parameters(ir, function);
    let Some(identities) = ir.function_parameter_identities(function) else {
        return false;
    };
    identities.len() == parameters.len()
        && identities.iter().all(|identity| match identity.role {
            crate::ir::IrParameterRole::ContextValue | crate::ir::IrParameterRole::Value => {
                identity.source_name.is_some()
            }
            crate::ir::IrParameterRole::AnonymousContextParameter { .. }
            | crate::ir::IrParameterRole::ContextReceiver { .. }
            | crate::ir::IrParameterRole::ExtensionReceiver
            | crate::ir::IrParameterRole::PropertySetterValue
            | crate::ir::IrParameterRole::Generated(
                crate::ir::IrGeneratedParameterRole::InterfaceDelegationValue { .. },
            ) => true,
            _ => false,
        })
        && ir.signatures.get(&function).is_none_or(|signature| {
            signature.type_params.iter().all(|parameter| {
                parameter
                    .bounds
                    .iter()
                    .all(|(bound, _)| ensure_determined_type(*bound).is_ok())
            })
        })
}

fn render_member_method_declaration(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    declaration: MemberMethodDeclaration,
    name: &str,
    owner: &str,
    type_context: &[crate::types::Ty],
) -> String {
    let (function, parameters) = match declaration {
        MemberMethodDeclaration::Source { function, .. }
        | MemberMethodDeclaration::CompanionBridge { function, .. } => {
            return render_source_member_method(ir, function, name, owner, type_context);
        }
        MemberMethodDeclaration::GeneratedDataMember { function, .. } => {
            return render_source_member_method(ir, function, name, owner, type_context);
        }
        MemberMethodDeclaration::InPlaceDelegate { function, .. } => {
            return render_generated_member_delegate(
                &ir.functions[function as usize],
                class.fq_name,
            )
            .expect("a renderable in-place delegate has determined JVM types");
        }
        MemberMethodDeclaration::GeneratedMethod { .. } => {
            unreachable!("generated methods use the generic platform-clash diagnostic")
        }
        MemberMethodDeclaration::DelegationForwarder { function, .. } => {
            return render_source_member_method(ir, function, name, owner, type_context);
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
                        type_context,
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
                        type_context,
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
                        type_context,
                    )
                }
            };
        }
        MemberMethodDeclaration::PropertyAnnotationMarker { function, .. } => (
            function,
            property_marker_diagnostics::marker_parameters(ir, function),
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
            &source_member_type_name_in(ty, type_context)
                .expect("a reported member clash has determined source types"),
        );
    }
    rendered.push_str("): ");
    let ret = ir
        .vc_declared_sigs
        .get(&function)
        .map(|(_, _, ret)| *ret)
        .unwrap_or(ir.functions[function as usize].ret);
    rendered.push_str(
        &source_member_type_name_in(ret, type_context)
            .expect("a reported member clash has a determined return type"),
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
    type_context: &[crate::types::Ty],
) -> String {
    let callable = if setter {
        format!("<set-{}>", property.name)
    } else {
        format!("<get-{}>", property.name)
    };
    if let Some(function) = function {
        return render_source_member_method(ir, function, &callable, owner, type_context);
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
                source_member_type_name_in(*ty, type_context)
                    .expect("a reported property-accessor clash has determined source types")
            )
        })
        .collect::<Vec<_>>();
    if setter {
        parameters.push(format!(
            "{}: {}",
            rendered_callable_name("<set-?>"),
            source_member_type_name_in(property.ty, type_context)
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
        source_member_type_name_in(ret, type_context)
            .expect("a reported property-accessor clash has a determined return type")
    )
}

fn render_source_member_method(
    ir: &IrFile,
    function: FunId,
    name: &str,
    owner: &str,
    type_context: &[crate::types::Ty],
) -> String {
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
        let ty = source_member_type_name_in(ty, type_context)
            .expect("a reported member clash has determined source types");
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
            crate::ir::IrParameterRole::PropertySetterValue => {
                values.push(format!("{}: {ty}", rendered_callable_name("<set-?>")));
                value_index += 1;
            }
            crate::ir::IrParameterRole::Generated(
                crate::ir::IrGeneratedParameterRole::InterfaceDelegationValue { .. },
            ) => {
                let name = crate::jvm::parameter_names::local_variable(
                    identity,
                    &ir.functions[function as usize].name,
                )
                .expect("an interface-delegation parameter has a JVM diagnostic spelling");
                values.push(format!("{}: {ty}", rendered_callable_name(&name)));
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
    let trailing_bounds = ir
        .signatures
        .get(&function)
        .map_or_else(Vec::new, |signature| {
            render_member_type_parameters(&mut rendered, &signature.type_params, type_context)
        });
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
        .or_else(|| {
            ir.signatures
                .get(&function)
                .and_then(|signature| signature.ret)
        })
        .unwrap_or(ir.functions[function as usize].ret);
    rendered.push_str(
        &source_member_type_name_in(ret, type_context)
            .expect("a reported member clash has a determined return type"),
    );
    render_member_where_clause(&mut rendered, &trailing_bounds, type_context);
    rendered.push_str(" defined in ");
    rendered.push_str(owner);
    rendered
}

fn render_member_type_parameters<'a>(
    rendered: &mut String,
    parameters: &'a [crate::ir::IrTypeParameter],
    type_context: &[crate::types::Ty],
) -> Vec<(&'a str, crate::types::Ty)> {
    if parameters.is_empty() {
        return Vec::new();
    }
    let mut trailing_bounds = Vec::new();
    rendered.push('<');
    for (index, parameter) in parameters.iter().enumerate() {
        if index > 0 {
            rendered.push_str(", ");
        }
        rendered.push_str(&rendered_callable_name(&parameter.name));
        let bounds = parameter
            .bounds
            .iter()
            .map(|(bound, _)| *bound)
            .filter(|bound| !is_default_bound(*bound))
            .collect::<Vec<_>>();
        if let Some((&first, remaining)) = bounds.split_first() {
            rendered.push_str(" : ");
            rendered.push_str(
                &source_member_type_name_in(first, type_context)
                    .expect("a reported member clash has determined type-parameter bounds"),
            );
            for &bound in remaining {
                trailing_bounds.push((parameter.name.as_str(), bound));
            }
        }
    }
    rendered.push_str("> ");
    trailing_bounds
}

fn render_member_where_clause(
    rendered: &mut String,
    trailing_bounds: &[(&str, crate::types::Ty)],
    type_context: &[crate::types::Ty],
) {
    if trailing_bounds.is_empty() {
        return;
    }
    rendered.push_str(" where ");
    for (index, (parameter, bound)) in trailing_bounds.iter().enumerate() {
        if index > 0 {
            rendered.push_str(", ");
        }
        rendered.push_str(&rendered_callable_name(parameter));
        rendered.push_str(" : ");
        rendered.push_str(
            &source_member_type_name_in(*bound, type_context)
                .expect("a reported member clash has determined type-parameter bounds"),
        );
    }
}

fn source_member_type_name_in(
    ty: crate::types::Ty,
    type_context: &[crate::types::Ty],
) -> Result<String, UndeterminedDiagnosticType> {
    ensure_determined_type(ty)?;
    Ok(
        ty.source_name_with_type_parameter_in(type_context, &|name| {
            rendered_callable_name(crate::types::type_parameter_source_name(name))
        }),
    )
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

fn reject_duplicate_constructors(ir: &IrFile, class: &crate::ir::IrClass) -> Result<(), String> {
    let mut descriptors = crate::fx_hash::HashSet::default();
    if class.has_primary_ctor {
        descriptors.insert(crate::jvm::names::method_descriptor(
            &crate::jvm::ir_emit::class_ctor_jvm_tys(ir, class),
            crate::types::Ty::Unit,
        ));
    }
    for constructor in &class.secondary_ctors {
        let parameters = crate::jvm::ir_emit::secondary_prefix_jvm_tys(ir, class, constructor)
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

/// Whether the exact declaration-clash inventory above already owns this method identity.
///
/// Compiler passes may append representation-only helpers to `class.methods` without a source
/// signature. Those still need the conservative duplicate-method fallback. Source functions,
/// property accessors, annotation markers, generated data-class declarations, and recorded
/// in-place delegate methods all have a semantic declaration shape and must not also produce that
/// fallback's generic diagnostic.
fn member_method_is_inventoried(
    ir: &IrFile,
    class_id: crate::ir::ClassId,
    class: &crate::ir::IrClass,
    function: FunId,
) -> bool {
    ir.fn_signature_offsets.contains_key(&function)
        || (ir.is_data_class_member(class.fq_name_id(), function)
            && ir.class_diagnostic_span(class_id).is_some())
        || ir.is_jvm_delegate_method(function)
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

fn render_generated_member_delegate(
    function: &crate::ir::IrFunction,
    owner: crate::types::TypeName,
) -> Result<String, UndeterminedDiagnosticType> {
    render_physical_function(
        function,
        (0..function.params.len()).map(|_| "<this>"),
        &owner.render().replace(['/', '$'], "."),
    )
}

fn render_physical_function<'a>(
    function: &crate::ir::IrFunction,
    parameter_names: impl IntoIterator<Item = &'a str>,
    owner: &str,
) -> Result<String, UndeterminedDiagnosticType> {
    determined_function_signature(function)?;
    let names = parameter_names.into_iter().collect::<Vec<_>>();
    if names.len() != function.params.len() {
        return Err(UndeterminedDiagnosticType::InvalidContextParameterCount);
    }
    let mut rendered = format!("fun {}(", rendered_callable_name(&function.name));
    for (index, (name, ty)) in names.into_iter().zip(&function.params).enumerate() {
        if index > 0 {
            rendered.push_str(", ");
        }
        rendered.push_str(&rendered_callable_name(name));
        rendered.push_str(": ");
        rendered.push_str(&source_type_name(*ty)?);
    }
    rendered.push_str("): ");
    rendered.push_str(&source_type_name(function.ret)?);
    rendered.push_str(" defined in ");
    rendered.push_str(owner);
    Ok(rendered)
}

fn rendered_callable_name(name: &str) -> String {
    let mut characters = name.chars();
    let ordinary = characters
        .next()
        .is_some_and(crate::names::is_identifier_start)
        && characters.all(crate::names::is_identifier_continue)
        && name != "_"
        && !matches!(
            name,
            "as" | "break"
                | "class"
                | "continue"
                | "do"
                | "else"
                | "false"
                | "for"
                | "fun"
                | "if"
                | "in"
                | "interface"
                | "is"
                | "null"
                | "object"
                | "package"
                | "return"
                | "super"
                | "this"
                | "throw"
                | "true"
                | "try"
                | "typealias"
                | "typeof"
                | "val"
                | "var"
                | "when"
                | "while"
        );
    if ordinary {
        name.to_string()
    } else {
        format!("`{name}`")
    }
}

fn is_default_bound(bound: crate::types::Ty) -> bool {
    matches!(bound.kind(), crate::types::TyKind::Nullable(inner)
        if matches!(inner.kind(), crate::types::TyKind::Obj(name, arguments)
            if arguments.is_empty() && name == crate::types::wk::any()))
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
    match ty.kind() {
        TyKind::Error => Err(UndeterminedDiagnosticType::Error),
        TyKind::Pending => Err(UndeterminedDiagnosticType::Pending),
        TyKind::Obj(_, arguments) => {
            for &argument in arguments {
                ensure_determined_type(argument)?;
            }
            Ok(())
        }
        TyKind::Fun(signature) => {
            for &parameter in &signature.params {
                ensure_determined_type(parameter)?;
            }
            ensure_determined_type(signature.ret)
        }
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::InProjection(inner)
        | TyKind::OutProjection(inner)
        | TyKind::StarProjection(inner)
        | TyKind::TyParam(_, inner) => ensure_determined_type(inner),
        TyKind::Intersection(parts) => {
            for &part in parts {
                ensure_determined_type(part)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn qualified_type_name(ty: crate::types::Ty) -> Result<String, UndeterminedDiagnosticType> {
    Ok(match ty.kind() {
        TyKind::INT => "kotlin.Int".to_string(),
        TyKind::BYTE => "kotlin.Byte".to_string(),
        TyKind::SHORT => "kotlin.Short".to_string(),
        TyKind::LONG => "kotlin.Long".to_string(),
        TyKind::FLOAT => "kotlin.Float".to_string(),
        TyKind::DOUBLE => "kotlin.Double".to_string(),
        TyKind::BOOLEAN => "kotlin.Boolean".to_string(),
        TyKind::CHAR => "kotlin.Char".to_string(),
        TyKind::UBYTE => "kotlin.UByte".to_string(),
        TyKind::USHORT => "kotlin.UShort".to_string(),
        TyKind::UINT => "kotlin.UInt".to_string(),
        TyKind::ULONG => "kotlin.ULong".to_string(),
        TyKind::STRING => "kotlin.String".to_string(),
        TyKind::Unit => "kotlin.Unit".to_string(),
        TyKind::Nothing => "kotlin.Nothing".to_string(),
        TyKind::Null => "null".to_string(),
        TyKind::Obj(name, arguments) => {
            let base = name.render().replace(['/', '$'], ".");
            if arguments.is_empty() || ty.is_raw_application() {
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
        TyKind::Nullable(inner) => {
            let rendered = qualified_type_name(inner)?;
            if inner.is_fun() {
                format!("({rendered})?")
            } else {
                format!("{rendered}?")
            }
        }
        TyKind::PlatformNullable(inner) => format!("{}!", qualified_type_name(inner)?),
        TyKind::MutabilityFlexible(inner) => qualified_type_name(inner)?,
        TyKind::DefinitelyNotNull(inner) => {
            format!("{} & Any", qualified_type_name(inner)?)
        }
        TyKind::Intersection(parts) => parts
            .iter()
            .map(|part| qualified_type_name(*part))
            .collect::<Result<Vec<_>, _>>()?
            .join(" & "),
        TyKind::TyParam(name, _) => {
            crate::types::type_parameter_source_name(name.as_str()).to_string()
        }
        TyKind::Fun(signature) => {
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
        TyKind::InProjection(inner) => format!("in {}", qualified_type_name(inner)?),
        TyKind::OutProjection(inner) => format!("out {}", qualified_type_name(inner)?),
        TyKind::StarProjection(_) => "*".to_string(),
        TyKind::RawProjection(inner) => qualified_type_name(inner)?,
        TyKind::Error => return Err(UndeterminedDiagnosticType::Error),
        TyKind::Pending => return Err(UndeterminedDiagnosticType::Pending),
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

    fn platform_clash_rows(
        ir: &IrFile,
        override_results: &crate::jvm::override_results::OverrideResults,
    ) -> Vec<(Span, String)> {
        let mut diagnostics = DiagSink::new();
        assert!(report_platform_declaration_clashes(
            ir,
            override_results,
            &mut diagnostics,
        ));
        diagnostics
            .diags
            .into_iter()
            .map(|diagnostic| (diagnostic.span, diagnostic.msg))
            .collect()
    }

    #[test]
    fn uninventoried_generated_method_still_clashes_with_source_method() {
        let mut ir = IrFile::default();
        let owner = crate::types::type_name("fixtures/Owner");
        let source = ir.add_fun(member_function(owner, "getValue"));
        let generated_accessor = ir.add_fun(member_function(owner, "getValue"));
        ir.fn_signature_offsets.insert(source, 11);

        let mut class = crate::ir::IrClass::synthetic(owner);
        class.methods = vec![source, generated_accessor];
        let class_id = ir.add_class(class);

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
        let message = "platform declaration clash: The following declarations have the same JVM signature (getValue()I):";
        assert_eq!(
            platform_clash_rows(
                &ir,
                &crate::jvm::override_results::OverrideResults::default(),
            ),
            vec![
                (Span::new(0, 0), message.to_string()),
                (Span::new(11, 11), message.to_string()),
            ],
        );
    }

    #[test]
    fn uninventoried_accessor_compares_boxed_override_descriptor() {
        let mut ir = IrFile::default();
        let owner = crate::types::type_name("fixtures/Owner");
        let boxed_override = ir.add_fun(member_function(owner, "getValue"));
        let mut generated = member_function(owner, "getValue");
        generated.ret = crate::types::Ty::nullable(crate::types::Ty::Int);
        let generated_accessor = ir.add_fun(generated);
        ir.fn_signature_offsets.insert(boxed_override, 11);

        let mut class = crate::ir::IrClass::synthetic(owner);
        class.methods = vec![boxed_override, generated_accessor];
        class.sam_wrapper = Some(crate::ir::IrSamWrapperClass {
            interface: crate::types::type_name("fixtures/GenericGetter"),
            method: boxed_override,
            boxes_primitive_result: true,
            suspend_arity: None,
            public_inline: false,
            function_adapter: false,
        });
        ir.add_class(class);

        let override_results = crate::jvm::override_results::box_scalar_override_results(
            &ir,
            &crate::backend::CheckedBackendCallables::default(),
            &crate::jvm::property_realizations::PropertyRealizations::default(),
        )
        .expect("the generated SAM wrapper has a complete physical result plan");
        assert_eq!(
            crate::jvm::ir_emit::function_descriptor(&ir, &override_results, boxed_override),
            "()Ljava/lang/Integer;"
        );
        let message = "platform declaration clash: The following declarations have the same JVM signature (getValue()Ljava/lang/Integer;):";
        assert_eq!(
            platform_clash_rows(&ir, &override_results),
            vec![
                (Span::new(0, 0), message.to_string()),
                (Span::new(11, 11), message.to_string()),
            ],
        );
    }

    #[test]
    fn delegation_forwarder_renders_provider_owned_parameter_ordinals() {
        let mut ir = IrFile::default();
        let owner = crate::types::type_name("fixtures/Owner");
        let mut delegated = member_function(owner, "value");
        delegated.params = vec![crate::types::Ty::Int];
        let bound = crate::types::Ty::obj("kotlin/CharSequence");
        let parameter = crate::types::Ty::ty_param("T value", bound);
        let comparable = crate::types::Ty::obj_args("kotlin/Comparable", &[parameter]);
        delegated.ret = bound;
        let delegated = ir.add_fun(delegated);
        ir.fn_params.insert(
            delegated,
            crate::ir::FnParamInfo::identities(vec![crate::ir::IrParameterIdentity::generated(
                crate::ir::IrGeneratedParameterRole::InterfaceDelegationValue { ordinal: 0 },
                None,
            )]),
        );
        ir.signatures.insert(
            delegated,
            crate::ir::IrGenericSig {
                type_params: vec![crate::ir::IrTypeParameter {
                    name: "T value".to_string(),
                    semantic_name: "T value".to_string(),
                    bounds: vec![(bound, false), (comparable, true)],
                    variance: crate::types::TypeVariance::Invariant,
                    reified: false,
                }],
                params: vec![crate::types::Ty::Int],
                ret: Some(parameter),
                supers: Vec::new(),
            },
        );

        let class = crate::ir::IrClass::synthetic(owner);
        assert!(member_method_declaration_is_renderable(
            &ir,
            &class,
            MemberMethodDeclaration::DelegationForwarder {
                offset: 0,
                function: delegated,
            },
        ));
        assert_eq!(
            render_source_member_method(
                &ir,
                delegated,
                "value",
                "fixtures.Owner",
                &[crate::types::Ty::Int, parameter, bound, comparable],
            ),
            "fun <`T value` : CharSequence> value(p0: Int): `T value` where `T value` : Comparable<`T value`> defined in fixtures.Owner"
        );
    }

    #[test]
    fn cps_delegation_forwarder_uses_generic_clash_diagnostic() {
        let mut ir = IrFile::default();
        let owner = crate::types::type_name("fixtures/Owner");
        let continuation = crate::types::Ty::obj_args(
            "kotlin/coroutines/Continuation",
            &[crate::types::Ty::String],
        );
        let erased_result =
            crate::types::Ty::nullable(crate::types::Ty::obj_name(crate::types::wk::any()));
        let mut source = member_function(owner, "fetch");
        source.params = vec![crate::types::Ty::Int, continuation];
        source.ret = erased_result;
        let source = ir.add_fun(source);
        let mut delegated = member_function(owner, "fetch");
        delegated.params = vec![crate::types::Ty::Int, continuation];
        delegated.ret = erased_result;
        let delegated = ir.add_fun(delegated);
        ir.fn_signature_offsets.insert(source, 11);
        ir.fn_params.insert(
            source,
            crate::ir::FnParamInfo::source_names(vec![
                "value".to_string(),
                "continuation".to_string(),
            ]),
        );
        ir.fn_params.insert(
            delegated,
            crate::ir::FnParamInfo::identities(vec![
                crate::ir::IrParameterIdentity::generated(
                    crate::ir::IrGeneratedParameterRole::InterfaceDelegationValue { ordinal: 0 },
                    None,
                ),
                crate::ir::IrParameterIdentity::generated(
                    crate::ir::IrGeneratedParameterRole::Continuation,
                    None,
                ),
            ]),
        );
        ir.interface_delegation_forwarders.insert(delegated);

        let mut class = crate::ir::IrClass::synthetic(owner);
        class.methods = vec![source, delegated];
        ir.add_class(class);

        let descriptor = "(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;";
        let message = format!(
            "platform declaration clash: The following declarations have the same JVM signature (fetch{descriptor}):"
        );
        assert_eq!(
            platform_clash_rows(
                &ir,
                &crate::jvm::override_results::OverrideResults::default(),
            ),
            vec![
                (Span::new(0, 0), message.clone()),
                (Span::new(11, 11), message),
            ]
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
        assert!(!is_default_bound(crate::types::Ty::obj_name(
            crate::types::wk::any()
        )));
        assert!(!is_default_bound(crate::types::Ty::obj("repo/Any")));
    }

    #[test]
    fn diagnostic_identifiers_escape_hard_keywords_and_bare_underscore() {
        assert_eq!(rendered_callable_name("class"), "`class`");
        assert_eq!(rendered_callable_name("_"), "`_`");
        assert_eq!(rendered_callable_name("value"), "value");
        assert_eq!(rendered_callable_name("T value"), "`T value`");
        assert_eq!(rendered_callable_name("π٢"), "π٢");
        assert_eq!(rendered_callable_name("Ⅻ"), "`Ⅻ`");
        assert_eq!(rendered_callable_name("²"), "`²`");
    }
}
