//! The expression scopes a value-class rewrite walks. A function body, a lambda's standalone
//! implementation, and a lambda's retained inline body each number their value slots on their own,
//! so a walk from one never enters another.

use super::inline_body_slots::OwnParameters;
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{ExprId, IrExpr, IrFile};
use crate::types::Ty;

/// Collect every `ExprId` reachable from `root` (a function body), so rewrites stay within bodies that
/// own value-class values unboxed.
pub(super) fn collect_reachable(exprs: &[IrExpr], root: ExprId, out: &mut HashSet<ExprId>) {
    if !out.insert(root) {
        return;
    }
    crate::ir::for_each_child(exprs, root, &mut |c| collect_reachable(exprs, c, out));
}

/// Add every retained lambda inline body reachable from `bodies` as its own lexical value-slot scope.
/// The standalone `impl_fn` body and the value-producing `inline_body` are distinct expression DAGs,
/// but both use the lambda implementation's numbering. JVM inline splicing can emit the latter even
/// when the implementation method is also emitted, so representation rewrites must visit both copies.
pub(super) fn append_inline_body_scopes(
    ir: &IrFile,
    bodies: &mut Vec<(ExprId, HashMap<u32, Ty>)>,
    slot_types: &[HashMap<u32, Ty>],
    own_parameters: &OwnParameters,
) {
    let mut known_roots: HashSet<ExprId> = bodies.iter().map(|(root, _)| *root).collect();
    let mut cursor = 0;
    while cursor < bodies.len() {
        let root = bodies[cursor].0;
        cursor += 1;
        let mut reachable = HashSet::default();
        collect_reachable_scoped(&ir.exprs, root, &mut reachable);
        for expression in reachable {
            let IrExpr::Lambda {
                impl_fn,
                inline_body: Some(inline_body),
                ..
            } = &ir.exprs[expression as usize]
            else {
                continue;
            };
            if known_roots.insert(*inline_body) {
                if let Some(slots) = slot_types.get(*impl_fn as usize) {
                    let slots = own_parameters.unboxed(*impl_fn, slots.clone());
                    bodies.push((*inline_body, slots));
                }
            }
        }
    }
}

/// Like [`collect_reachable], but never descends into a lambda's body — only its captures. Both a
/// closure's standalone implementation body and its retained inline body have their own value-index
/// numbering and slot types. Reaching either from the enclosing scope would let representation
/// analysis interpret those indices using the wrong lexical frame. Callers that also transform
/// retained inline bodies add them as independent roots with [`append_inline_body_scopes`].
pub(super) fn collect_reachable_scoped(exprs: &[IrExpr], root: ExprId, out: &mut HashSet<ExprId>) {
    if !out.insert(root) {
        return;
    }
    if let IrExpr::Lambda { captures, .. } = &exprs[root as usize] {
        for &c in captures {
            collect_reachable_scoped(exprs, c, out);
        }
        return;
    }
    crate::ir::for_each_child(exprs, root, &mut |c| {
        collect_reachable_scoped(exprs, c, out)
    });
}
