//! Same-file calls to `companion { … }` block members.
//!
//! A block member is a static member of the classifier that declared the block (kotlinc places it
//! among that class's static methods; a written `companion fun C.f()` stays on the file facade).
//! Its checked call is lowered like any same-file callable, as a facade-relative `Local` shape.
//! Once every body is lowered, this pass names the declaring class as the static owner of each such
//! call, using the placement recorded when the function was declared. It performs no lookup.

use crate::ir::{Callee, IrExpr, IrFile};

pub(super) fn realize_companion_block_calls(ir: &mut IrFile) {
    if !ir.companion_blocks.has_functions() {
        return;
    }
    for expression in 0..ir.exprs.len() {
        let IrExpr::Call { callee, .. } = &ir.exprs[expression] else {
            continue;
        };
        let owner_of = |function: &u32| {
            ir.companion_blocks
                .declaring_class(*function)
                .map(|class| ir.classes[class as usize].fq_name_id())
        };
        let replacement = match callee {
            Callee::Local(function) => owner_of(function).map(|owner| Callee::ClassStatic {
                owner,
                function: *function,
            }),
            Callee::LocalDefault(function) => {
                owner_of(function).map(|owner| Callee::ClassStaticDefault {
                    owner,
                    function: *function,
                })
            }
            Callee::LocalWithDefaults { function, defaults } => {
                owner_of(function).map(|owner| Callee::ClassStaticWithDefaults {
                    owner,
                    function: *function,
                    defaults: defaults.clone(),
                })
            }
            _ => None,
        };
        if let (Some(replacement), IrExpr::Call { callee, .. }) =
            (replacement, &mut ir.exprs[expression])
        {
            *callee = replacement;
        }
    }
}
