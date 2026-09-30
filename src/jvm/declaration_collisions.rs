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
    diags: &mut DiagSink,
) -> Result<(), ()> {
    let platform_clash = report_platform_declaration_clashes(ir, override_results, diags);
    for class in &ir.classes {
        if let Err(message) = reject_duplicate_constructors(class) {
            diags.error(Span::new(0, 0), message);
            return Err(());
        }
        let Some(properties) = ir.member_ext_props.get(&class.fq_name) else {
            continue;
        };
        for property in properties {
            for accessor in std::iter::once(property.getter).chain(property.setter) {
                if let Err(message) = reject_duplicate_method(ir, class, accessor) {
                    diags.error(Span::new(0, 0), message);
                    return Err(());
                }
            }
        }
    }
    if platform_clash {
        Err(())
    } else {
        Ok(())
    }
}

/// Source methods whose physical JVM name and descriptor are identical. Kotlin already rejected
/// members that share a declaration signature, so a group here differs in Kotlin and collides only
/// after erasure. Methods without a recorded `fun` offset are representations, not declarations.
fn report_platform_declaration_clashes(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    diags: &mut DiagSink,
) -> bool {
    let mut clashes = Vec::new();
    for class in &ir.classes {
        let mut groups: std::collections::HashMap<(String, String), Vec<u32>> =
            std::collections::HashMap::new();
        for &method in &class.methods {
            let Some(&offset) = ir.fn_signature_offsets.get(&method) else {
                continue;
            };
            let Some(function) = ir.functions.get(method as usize) else {
                continue;
            };
            let descriptor = crate::jvm::ir_emit::function_descriptor(ir, override_results, method);
            groups
                .entry((function.name.clone(), descriptor))
                .or_default()
                .push(offset);
        }
        for ((name, descriptor), offsets) in groups {
            if offsets.len() < 2 {
                continue;
            }
            let message = format!(
                "platform declaration clash: The following declarations have the same JVM signature ({name}{descriptor}):"
            );
            for offset in offsets {
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

fn reject_duplicate_constructors(class: &crate::ir::IrClass) -> Result<(), String> {
    let mut descriptors = std::collections::HashSet::new();
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

fn reject_duplicate_method(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    accessor: FunId,
) -> Result<(), String> {
    let Some(accessor_function) = ir.functions.get(accessor as usize) else {
        return Err("internal error: member extension accessor has no JVM function".to_string());
    };
    let descriptor =
        crate::jvm::names::method_descriptor(&accessor_function.params, accessor_function.ret);
    if class.methods.iter().copied().any(|candidate| {
        candidate != accessor
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
