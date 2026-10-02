//! The trivial cleanups kotlinc's temporaries pass starts with: a value loaded only to be
//! discarded, and a `nop` next to an instruction it does not need to stand in for.

use super::adjacency::Body;
use super::shapes::{is_op, var_op, VarOp, NOP, POP, POP2};
use crate::jvm::method_node::Insn;

/// Replace every `xload; pop` (`pop2` for a two-word value) with no kept label between the two by
/// a `nop`, as kotlinc's `simplifyTrivialInstructions` does; `true` when there was one. The `nop`
/// stands where the load stood, so [`remove_nops`] keeps it exactly when kotlinc's sweep does: the
/// only instruction of a line (a discarded read `value` written as a statement) or the start of a
/// protected range.
pub(super) fn remove_discarded_loads(body: &mut Body) -> bool {
    let mut removed = false;
    let mut cursor = body.first_insn();
    while let Some(load) = cursor {
        let Some(pop) = body.next_insn(load) else {
            break;
        };
        let pair = match var_op(body.insn(load)) {
            Some(VarOp::Load(kind, _)) if kind.words() == 1 => is_op(body.insn(pop), POP),
            Some(VarOp::Load(kind, _)) if kind.words() == 2 => is_op(body.insn(pop), POP2),
            _ => false,
        };
        if !pair || body.labelled(pop) {
            cursor = Some(pop);
            continue;
        }
        removed = true;
        body.replace(load, Insn::Op(NOP));
        body.remove(pop);
        cursor = body.next_insn(load);
    }
    removed
}

/// kotlinc's second trivial sweep: a `nop` goes when no kept label separates it from the
/// instruction on one side, unless a protected range starts at it; `true` when one went.
pub(super) fn remove_nops(body: &mut Body) -> bool {
    let mut removed = false;
    let mut cursor = body.first_insn();
    while let Some(nop) = cursor {
        let next = body.next_insn(nop);
        if !is_op(body.insn(nop), NOP) {
            cursor = next;
            continue;
        }
        let meaningful_before = body.prev_insn(nop).is_some() && !body.labelled(nop);
        let meaningful_after = next.is_some_and(|next| !body.labelled(next));
        if !body.protected_start(nop) && (meaningful_before || meaningful_after) {
            body.remove(nop);
            removed = true;
        }
        cursor = next;
    }
    removed
}
