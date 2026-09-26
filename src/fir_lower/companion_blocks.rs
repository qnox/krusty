//! The placement of `companion { … }` block members, and same-file calls to them.
//!
//! A block member is a static member of the classifier that declared the block (kotlinc places it
//! among that class's static methods; a written `companion fun C.f()` stays on the file facade).
//! [`static_placement`] fixes that placement once per declaration, for same-file and module
//! declarations alike. A checked same-file call is lowered like any same-file callable, as a
//! facade-relative `Local` shape; once every body is lowered, [`realize_companion_block_calls`]
//! names the declaring class as the static owner of each such call, using the placement recorded
//! when the function was declared. Neither performs a lookup.

use crate::fir::DeclarationFlags;
use crate::ir::{Callee, ClassId, IrExpr, IrFile, IrStaticPlacement};
use crate::types::Ty;

use super::FirFileLoweringFailure;

/// The placement of a declaration with no dispatch owner, from its checked header `flags` and its
/// checked `associated_receiver`: a block member's receiver is the resolved classifier that
/// declared its block. A flagged member without one is an invalid checked declaration and fails
/// with `failure`; it is never placed in the package instead.
pub(super) fn static_placement(
    flags: DeclarationFlags,
    associated_receiver: Option<Ty>,
    failure: FirFileLoweringFailure,
) -> Result<IrStaticPlacement, FirFileLoweringFailure> {
    if !flags.has(DeclarationFlags::COMPANION_BLOCK_MEMBER) {
        return Ok(IrStaticPlacement::Package);
    }
    associated_receiver
        .and_then(|receiver| receiver.non_null().obj_internal())
        .map(|declaring_class| IrStaticPlacement::CompanionBlock { declaring_class })
        .ok_or(failure)
}

/// The same-file class realizing a block member placed by `placement`; `None` for a package
/// declaration. A declaring class absent from this file fails with `failure`.
pub(super) fn declaring_class_in_file(
    ir: &IrFile,
    placement: IrStaticPlacement,
    failure: FirFileLoweringFailure,
) -> Result<Option<ClassId>, FirFileLoweringFailure> {
    match placement {
        IrStaticPlacement::Package => Ok(None),
        IrStaticPlacement::CompanionBlock { declaring_class } => ir
            .class_id_by_name(declaring_class)
            .map(Some)
            .ok_or(failure),
    }
}

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
