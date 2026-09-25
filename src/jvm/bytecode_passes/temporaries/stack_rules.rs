//! The rules that keep a temporary's value on the operand stack instead of in its local:
//!
//! - a temporary never loaded is stored as a `pop`/`pop2` instead;
//! - a temporary loaded once, with nothing that executes between its store and that load, is
//!   removed together with the load;
//! - `astore t; aload y; aload t` becomes `aload y; swap`, and `astore t; getstatic f; aload t` (a
//!   one-word `f`) becomes `getstatic f; swap`;
//! - `astore t; aload t; ldc "…"; invokestatic Intrinsics.checkNotNullExpressionValue; aload t`
//!   becomes `dup; ldc "…"; invokestatic …`.

use super::super::insn_list::NodeId;
use super::adjacency::Body;
use super::shapes::{
    is_expression_null_check, is_op, is_string_constant, is_swappable, var_op, Kind, VarOp, DUP,
    NOP, POP, POP2, SWAP,
};
use crate::jvm::method_node::Insn;

/// Apply the rules to each temporary store and the loads that read it, in store order; `true` when
/// one applied.
pub(super) fn apply(body: &mut Body, temporaries: Vec<(NodeId, Vec<NodeId>)>) -> bool {
    let mut changed = false;
    for (store, loads) in temporaries {
        if !body.has(store) || loads.iter().any(|&load| !body.has(load)) {
            continue;
        }
        let Some(VarOp::Store(kind, _)) = var_op(body.insn(store)) else {
            continue;
        };
        changed |= match loads[..] {
            [] => {
                body.replace(store, Insn::Op(if kind.words() == 2 { POP2 } else { POP }));
                true
            }
            [load] => keep_single_load(body, store, load, kind),
            [first, last] => keep_expression_check(body, store, first, last, kind),
            _ => false,
        };
    }
    changed
}

/// A temporary read once: gone with its load when nothing executes between them, or swapped into
/// place under one value loaded in between.
fn keep_single_load(body: &mut Body, store: NodeId, load: NodeId, kind: Kind) -> bool {
    // Nothing that executes between the store and its load: only `nop`s, and no label any control
    // flow reaches.
    if body.precedes(store, load) {
        let mut adjacent = true;
        let mut cursor = body.next_insn(store);
        while let Some(next) = cursor {
            if body.arrived(next) || (next != load && !is_op(body.insn(next), NOP)) {
                adjacent = false;
                break;
            }
            if next == load {
                break;
            }
            cursor = body.next_insn(next);
        }
        if adjacent {
            body.remove(store);
            body.remove(load);
            return true;
        }
    }
    if kind != Kind::Reference {
        return false;
    }
    let Some([_, middle, third]) = body
        .raw_sequence(store, 3)
        .and_then(|ids| <[NodeId; 3]>::try_from(ids).ok())
    else {
        return false;
    };
    if third != load || !is_swappable(body.insn(middle)) {
        return false;
    }
    body.insert_after(middle, Insn::Op(SWAP));
    body.remove(store);
    body.remove(load);
    true
}

/// `astore t; aload t; ldc "…"; invokestatic checkNotNullExpressionValue; aload t`: the checked
/// value is duplicated instead of stored.
fn keep_expression_check(
    body: &mut Body,
    store: NodeId,
    first: NodeId,
    last: NodeId,
    kind: Kind,
) -> bool {
    if kind != Kind::Reference {
        return false;
    }
    let Some([_, load, ldc, check, reload]) = body
        .raw_sequence(store, 5)
        .and_then(|ids| <[NodeId; 5]>::try_from(ids).ok())
    else {
        return false;
    };
    if load != first
        || reload != last
        || !is_string_constant(body.insn(ldc))
        || !is_expression_null_check(body.insn(check))
    {
        return false;
    }
    body.insert_before(ldc, Insn::Op(DUP));
    body.remove(store);
    body.remove(first);
    body.remove(last);
    true
}
