//! JVM declaration-signature validation after representation selection.
//!
//! Kotlin declarations can be distinct at the source level while their JVM realizations collide.
//! This pass runs only after representation passes have selected physical method names and types,
//! so FIR and common lowering never depend on JVM descriptors.

use crate::ir::{FunId, IrFile};

pub(super) fn validate(ir: &IrFile) -> Result<(), String> {
    for class in &ir.classes {
        let Some(properties) = ir.member_ext_props.get(&class.fq_name) else {
            continue;
        };
        for property in properties {
            for accessor in std::iter::once(property.getter).chain(property.setter) {
                reject_duplicate_method(ir, class, accessor)?;
            }
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
