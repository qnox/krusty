//! Physical ownership of local declarations after a lambda becomes a class.

use crate::ir::{Callee, ClassId, FunId, IrExpr, IrFile};

/// Move the declarations selected by stable [`FunId`] into `class` and retarget every static call
/// to the same identities. Selection belongs to the ordinary- or suspend-lambda realization that
/// knows its lifting scope; physical owner mutation is shared so neither path can move a method
/// without moving its calls.
pub(super) fn move_lifted_declarations_to_class(
    ir: &mut IrFile,
    class: ClassId,
    declared: &[FunId],
) {
    if declared.is_empty() {
        return;
    }
    let mut declared = declared.to_vec();
    declared.sort_unstable();
    declared.dedup();
    let owner = ir.classes[class as usize].fq_name;
    for &function in &declared {
        for other in &mut ir.classes {
            other.methods.retain(|&method| method != function);
        }
        ir.class_method_owners.remove(&function);
        ir.classes[class as usize].methods.push(function);
        ir.note_class_method(class, function);
        ir.class_static_local_functions.insert(function, owner);
    }
    for expression in &mut ir.exprs {
        let IrExpr::Call {
            callee:
                Callee::ClassStatic {
                    owner: called,
                    function,
                }
                | Callee::ClassStaticWithDefaults {
                    owner: called,
                    function,
                    ..
                }
                | Callee::ClassStaticDefault {
                    owner: called,
                    function,
                },
            ..
        } = expression
        else {
            continue;
        };
        if declared.binary_search(function).is_ok() {
            *called = owner;
        }
    }
}
