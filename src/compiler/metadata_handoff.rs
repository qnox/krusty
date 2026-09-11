//! Checked declaration metadata carried from the transient active source unit into common IR.
//!
//! Checked-FIR lowering owns executable semantics and intentionally never sees source spelling.
//! Kotlin metadata still needs declaration-only facts such as checked annotations, type-alias
//! abbreviations, and an explicit definitely-non-null occurrence. This handoff joins those
//! already-checked sidecars to stable declaration identities realized in common IR; it performs no
//! name lookup, overload selection, inference, or semantic fallback.

use crate::ast::File;
use crate::fir::{
    ActiveSourceDeclarations, DeclarationFlags, DeclarationId, DeclarationKind,
    ResolvedModuleIndex, SourceFileId,
};
use crate::ir::IrFile;

fn attach_generated_declaration_lines(
    class: crate::ir::ClassId,
    functions: &[crate::ir::FunId],
    line: u32,
    ir: &mut IrFile,
) {
    ir.classes[class as usize].decl_line = line;
    ir.classes[class as usize].decl_start_line = line;
    for &function in functions {
        ir.fn_decl_lines.insert(function, line);
        ir.fn_sig_lines.insert(function, line);
    }
}

/// Attach the annotated owner's source line to the exact companion and members that the frontend
/// generated for a plugin. The stable declaration graph is the ownership contract: neither the
/// plugin nor a backend needs to rediscover these declarations by name, class-vector scan, or an
/// absent-line heuristic after bodies have been generated.
fn attach_generated_companion_debug_metadata(
    owner: DeclarationId,
    owner_start_line: u32,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) {
    if owner_start_line == 0 {
        return;
    }
    let Some(companion) = index.companion_declaration(owner).filter(|declaration| {
        index
            .declaration_header(*declaration)
            .is_some_and(|header| header.flags.has(DeclarationFlags::COMPILER_GENERATED))
    }) else {
        return;
    };
    let Some(class) = ir.checked_classifier_classes.get(&companion).copied() else {
        return;
    };
    let functions = index
        .owned_declarations(companion)
        .iter()
        .filter_map(|&declaration| {
            let header = index.declaration_header(declaration)?;
            if header.kind != DeclarationKind::Function
                || !header.flags.has(DeclarationFlags::COMPILER_GENERATED)
            {
                return None;
            }
            index
                .callable_for_declaration(declaration)
                .and_then(|callable| ir.checked_callable_functions.get(&callable.id))
                .copied()
        })
        .collect::<Vec<_>>();
    attach_generated_declaration_lines(class, &functions, owner_start_line, ir);
}

/// Transfer line-only output metadata while one bounded Pass-2 parser unit is live. This belongs
/// to the stable metadata boundary rather than checked-FIR lowering: the lowerer never observes
/// source syntax.
pub(super) fn accept_active_debug_metadata(
    index: &ResolvedModuleIndex,
    file: &File,
    active: &ActiveSourceDeclarations,
    ir: &mut IrFile,
) {
    for declaration in active.stable_declarations() {
        if let Some((_, classifier)) = active.class(file, declaration) {
            if let Some(class) = ir.checked_classifier_classes.get(&declaration).copied() {
                ir.classes[class as usize].decl_line = classifier.decl_line;
                ir.classes[class as usize].decl_start_line = classifier.decl_start_line;
                if classifier.ctor_close_line != 0 {
                    ir.ctor_close_lines.insert(
                        ir.classes[class as usize].fq_name_id(),
                        classifier.ctor_close_line,
                    );
                }
                if classifier.body_close_line != 0 {
                    ir.class_close_lines.insert(
                        ir.classes[class as usize].fq_name_id(),
                        classifier.body_close_line,
                    );
                }
                attach_generated_companion_debug_metadata(
                    declaration,
                    classifier.decl_start_line,
                    index,
                    ir,
                );
            }
        }

        if let Some(function) = active.function(file, declaration) {
            if let Some(ir_function) = index
                .callable_for_declaration(declaration)
                .and_then(|callable| ir.checked_callable_functions.get(&callable.id).copied())
            {
                ir.fn_decl_lines.insert(ir_function, function.decl_line);
                ir.fn_sig_lines.insert(ir_function, function.sig_line);
                if function.body_close_line != 0 {
                    ir.fn_close_lines
                        .insert(ir_function, function.body_close_line);
                }
            }
        }

        let property_line = active
            .property(file, declaration)
            .map(|property| property.decl_line)
            .or_else(|| {
                active
                    .constructor_parameter(file, declaration)
                    .map(|property| property.decl_line)
            });
        if let Some(line) = property_line.filter(|line| *line != 0) {
            if let Some(property) = index.property_for_declaration(declaration) {
                if let Some(property) = ir.checked_properties.get_mut(&property) {
                    property.decl_line = line;
                }
            }
        }

        if let Some(entry) = active.enum_entry(file, declaration) {
            if let Some(owner) = index
                .declaration_anchor(declaration)
                .and_then(|anchor| anchor.owner)
                .and_then(|owner| ir.checked_classifier_classes.get(&owner))
                .copied()
            {
                if let Some(ir_entry) = ir.classes[owner as usize]
                    .enum_entries
                    .iter_mut()
                    .find(|candidate| candidate.name == entry.name)
                {
                    ir_entry.decl_line = entry.decl_line;
                }
            }
        }
    }
}

/// Attach declaration-only metadata for one already-bound source function. The stable declaration
/// is the join key for top-level functions, class members, and enum-entry members alike; source
/// ownership changes where the function is realized, not how its checked annotation payload is
/// transferred.
fn attach_function_metadata(
    function: &crate::ast::FunDecl,
    declaration: DeclarationId,
    info: &crate::resolve::TypeInfo,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) {
    let Some(callable) = index.callable_for_declaration(declaration) else {
        return;
    };
    let Some(&function_id) = ir.checked_callable_functions.get(&callable.id) else {
        return;
    };
    let annotations = crate::ir_lower::declaration_annotations(&function.annotations, info);
    if !annotations.is_empty() {
        ir.function_annotations.insert(function_id, annotations);
    }
    let mut parameter_annotations = function
        .params
        .iter()
        .map(|parameter| crate::ir_lower::value_parameter_annotations(&parameter.annotations, info))
        .collect::<Vec<_>>();
    if callable.shape.extension_receiver.is_some() {
        parameter_annotations.insert(
            (callable.shape.context_parameter_count as usize).min(parameter_annotations.len()),
            crate::ir::DeclarationAnnotations::default(),
        );
    }
    if parameter_annotations
        .iter()
        .any(|annotations| !annotations.is_empty())
    {
        ir.fn_param_annotations
            .insert(function_id, parameter_annotations);
    }
    if let Some(spelling) = index.declaration_spellings(declaration) {
        ir.fn_declared_spellings
            .insert(function_id, spelling.clone());
    }
}

/// Project declaration-owned inference policy for every function realized in one source file.
/// Unlike annotations that still migrate from active syntax, this fact is already part of the
/// finalized stable header and therefore also covers retained inline bodies that Pass 2 never
/// reparses as ordinary body work.
pub(super) fn attach_stable_function_inference_metadata(
    source: SourceFileId,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) {
    let functions = ir
        .checked_callable_functions
        .iter()
        .map(|(&callable, &function)| (callable, function))
        .collect::<Vec<_>>();
    for (callable, function) in functions {
        let Some(header) = index.callable(callable) else {
            continue;
        };
        if index
            .declaration_anchor(header.declaration)
            .is_none_or(|anchor| anchor.source != source)
        {
            continue;
        }
        let mut no_infer = (0..index.callable_parameter_name_count(callable))
            .map(|ordinal| {
                index
                    .callable_parameter(callable, ordinal as u32)
                    .is_some_and(|parameter| parameter.flags().is_no_infer())
            })
            .collect::<Vec<_>>();
        if header.shape.extension_receiver.is_some() {
            no_infer.insert(
                (header.shape.context_parameter_count as usize).min(no_infer.len()),
                false,
            );
        }
        if no_infer.iter().any(|flag| *flag) {
            ir.fn_param_no_infer.insert(function, no_infer);
        }
    }
}

pub(super) fn attach_checked_declaration_metadata(
    file: &File,
    active: &ActiveSourceDeclarations,
    info: &crate::resolve::TypeInfo,
    _source: SourceFileId,
    selected_root: DeclarationId,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) {
    if !ir.file_annotations_attached {
        let annotations = file
            .file_annotations
            .iter()
            .map(|(annotation, _)| annotation.clone())
            .collect::<Vec<_>>();
        ir.file_annotations = crate::ir_lower::declaration_annotations(&annotations, info);
        ir.file_annotations_attached = true;
    }
    if let Some(function) = active.function(file, selected_root) {
        attach_function_metadata(function, selected_root, info, index, ir);
    }
    let Some((source_class, class)) = active.class(file, selected_root) else {
        return;
    };
    let stable_class = selected_root;
    let Some(class_id) = ir.checked_classifier_classes.get(&stable_class).copied() else {
        return;
    };
    let classifier = ir.classes[class_id as usize].fq_name_id();
    let ir_class = &mut ir.classes[class_id as usize];
    ir_class.applied_annotations =
        crate::ir_lower::declaration_annotations(&class.annotations, info);
    ir_class.field_annotations = crate::ir_lower::class_field_annotations(class, info);
    ir_class.property_annotations = crate::ir_lower::class_property_annotations(class, info);
    ir_class.primary_ctor_annotations = crate::ir_lower::declaration_annotations(
        class
            .primary_ctor_annotations
            .as_deref()
            .unwrap_or_default(),
        info,
    );
    ir_class.ctor_param_annotations = crate::ir_lower::primary_constructor_parameter_annotations(
        class,
        info,
        ir_class.constructor_prefix_count as usize,
    );

    for (ordinal, constructor) in class.secondary_ctors.iter().enumerate() {
        let Some(declaration) = index.owned_declaration(
            stable_class,
            DeclarationKind::Constructor,
            u32::try_from(ordinal + 1).expect("too many source secondary constructors"),
        ) else {
            continue;
        };
        let Some(checked) = ir.checked_constructor_bodies.get_mut(&declaration) else {
            continue;
        };
        checked.annotations =
            crate::ir_lower::declaration_annotations(&constructor.annotations, info);
    }

    if let Some(header) = index.declaration_spellings(stable_class) {
        ir.class_declared_spellings
            .insert(classifier, header.clone());
    }

    let mut attach_property_spelling = |property: Option<DeclarationId>, name: &str| {
        if let Some(spelling) = property.and_then(|property| index.declaration_spellings(property))
        {
            ir.prop_declared_spellings
                .insert((classifier, name.to_owned()), spelling.clone());
        }
    };
    for (property_index, property) in class.props.iter().enumerate() {
        if property.is_property {
            attach_property_spelling(
                active.constructor_property_declaration(
                    source_class,
                    u32::try_from(property_index).expect("too many constructor properties"),
                ),
                &property.name,
            );
        }
    }
    for (property_index, property) in class.body_props.iter().enumerate() {
        attach_property_spelling(
            active.class_body_property_declaration(
                source_class,
                u32::try_from(property_index).expect("too many class properties"),
            ),
            &property.name,
        );
    }

    for (method, source_method) in class.methods.iter().enumerate() {
        let Some(stable_method) =
            index.owned_declaration(stable_class, DeclarationKind::Function, method as u32)
        else {
            continue;
        };
        attach_function_metadata(source_method, stable_method, info, index, ir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrClass, IrFunction};
    use crate::types::{type_name, Ty};

    #[test]
    fn generated_lines_update_only_the_selected_ir_identities() {
        let mut ir = IrFile::default();
        let selected_class = ir.add_class(IrClass::synthetic(type_name("x/Arbitrary")));
        let untouched_class = ir.add_class(IrClass::synthetic(type_name("x/Unrelated")));
        let function = || IrFunction {
            name: "generated".to_owned(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: None,
            is_static: false,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        };
        let selected_function = ir.add_fun(function());
        let untouched_function = ir.add_fun(function());

        attach_generated_declaration_lines(selected_class, &[selected_function], 7, &mut ir);

        assert_eq!(ir.classes[selected_class as usize].decl_line, 7);
        assert_eq!(ir.classes[selected_class as usize].decl_start_line, 7);
        assert_eq!(ir.classes[untouched_class as usize].decl_line, 0);
        assert_eq!(ir.classes[untouched_class as usize].decl_start_line, 0);
        assert_eq!(ir.fn_decl_lines.get(&selected_function), Some(&7));
        assert_eq!(ir.fn_sig_lines.get(&selected_function), Some(&7));
        assert!(!ir.fn_decl_lines.contains_key(&untouched_function));
        assert!(!ir.fn_sig_lines.contains_key(&untouched_function));
    }
}
