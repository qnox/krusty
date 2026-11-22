//! Gives the backend-neutral value-class adapters their JVM spelling.
//!
//! The lowering marks each representation boundary with a typed `Callee::ValueClassAdapter`: the
//! kind and the value class, nothing about how a platform names the operation. The JVM names them
//! `box-impl` (static, over the erased carrier) and `unbox-impl` (virtual on the box), so that
//! spelling, and the carrier descriptor, are decided here and nowhere earlier.

use super::*;

/// Replace every `Callee::ValueClassAdapter` with the JVM call that performs it.
pub(super) fn realize(ir: &mut IrFile, under: &Under) {
    for expression in &mut ir.exprs {
        let IrExpr::Call { callee, .. } = expression else {
            continue;
        };
        let Callee::ValueClassAdapter { adapter, owner } = **callee else {
            continue;
        };
        let carrier = desc(
            &under
                .get(&owner)
                .map(|ty| erase(ty, under))
                .unwrap_or(Ty::Error),
        );
        **callee = match adapter {
            ValueClassAdapter::Box => Callee::Static {
                owner,
                name: "box-impl".to_string(),
                descriptor: format!("({carrier})L{};", owner.render()),
                inline: InlineKind::None,
            },
            ValueClassAdapter::Unbox => Callee::realized_virtual(
                owner,
                "unbox-impl".to_string(),
                format!("(){carrier}"),
                None,
                false,
            ),
        };
    }
}
