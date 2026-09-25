//! kotlinc's null-check rules (`simplifyKnownSafeCallPatterns`): a checked value the path after the
//! check loads again stays on the stack instead.
//!
//! - `aload x; ifnonnull L; …; L: aload x` (L reached only by that jump) becomes
//!   `aload x; dup; ifnonnull L; pop; …; L:`;
//! - `aload x; ifnull L; aload x` becomes `aload x; dup; ifnull L`, when every jump to L has that
//!   shape and nothing falls into L, which then begins with a `pop`.
//!
//! Either load of `x` after the jump may instead follow an `aload y` or a one-word `getstatic`,
//! which a `swap` then puts under the kept value. The jump target's frame gains that value on its
//! stack.
//!
//! Several labels can stand where an `ifnull` jumps. One standing before the jump's own label is a
//! predecessor that is not such a jump, which defeats the rule; one standing after it is not a
//! predecessor of the `pop` at all: it lands after the `pop`, and so do the line number, the local
//! ranges and anything else that describes the instruction the label stood at, as kotlinc's
//! `L: pop; E:` has them. Those late labels are pinned: the `goto` and jump passes that run later
//! leave jumps to them alone, where kotlinc would have a line number in between.

use std::collections::BTreeSet;

use super::super::insn_list::NodeId;
use super::adjacency::Body;
use super::shapes::{
    ends_flow, is_swappable, loads_reference, null_jump, var_op, Kind, VarOp, DUP, IFNONNULL,
    IFNULL, POP, SWAP,
};
use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

/// Same 50 MiB-by-method complexity ceiling as kotlinc's optimizer. The product is deliberately
/// conservative for this representation: a slot/store analysis cell is larger than one byte.
pub(super) const ANALYSIS_COMPLEXITY_LIMIT: usize = 50 * 1024 * 1024;

pub(super) fn analysis_within_limit(instructions: usize, slots: usize, candidates: usize) -> bool {
    instructions
        .checked_mul(slots)
        .and_then(|cells| cells.checked_mul(candidates.max(1)))
        .is_some_and(|cells| cells <= ANALYSIS_COMPLEXITY_LIMIT)
}

/// An `ifnull` target the rules put a `pop` at: the `pop`, and the label the folded jumps name.
pub(super) struct NullTarget {
    pop: NodeId,
    label: LabelId,
}

/// What the rules did.
pub(super) struct Folds {
    /// Whether any load went.
    pub(super) folded: bool,
    pub(super) null_targets: Vec<NullTarget>,
}

/// The `aload x; ifnull|ifnonnull` pair starting at `load`, as `(slot, jump)`.
fn candidate(body: &Body, load: NodeId) -> Option<(u16, NodeId)> {
    let Some(VarOp::Load(Kind::Reference, slot)) = var_op(body.insn(load)) else {
        return None;
    };
    let jump = body.next_insn(load)?;
    null_jump(body.insn(jump))?;
    Some((slot, jump))
}

/// The fall-through after a null check reloads the checked slot: directly, or after an `aload` or a
/// one-word `getstatic` it can be swapped under. The reload, and the instruction a `swap` goes
/// after.
fn reload(body: &Body, from: Option<NodeId>, slot: u16) -> Option<(NodeId, Option<NodeId>)> {
    let from = from?;
    if loads_reference(body.insn(from), Some(slot)) {
        return Some((from, None));
    }
    if !is_swappable(body.insn(from)) {
        return None;
    }
    let [_, next] = body.raw_sequence(from, 2)?[..] else {
        unreachable!("a sequence of two");
    };
    loads_reference(body.insn(next), Some(slot)).then_some((next, Some(from)))
}

/// Every jump to the instruction `target` stands in front of, or `None` when something besides
/// jumps reaches it: a switch, a protected range, or the fall-through of the instruction in front
/// of it. Also that instruction.
fn only_jumps_to(body: &Body, target: LabelId) -> Option<(NodeId, Vec<NodeId>)> {
    let start = body.insn_at(target)?;
    if !body
        .prev_insn(start)
        .is_some_and(|prev| ends_flow(body.insn(prev)))
    {
        return None;
    }
    if body.protected_label(start) {
        return None;
    }
    let standing = body.labels_before(start);
    let mut jumps = Vec::new();
    for id in body.instructions() {
        match body.insn(id) {
            Insn::Jump { target, .. } if standing.contains(target) => jumps.push(id),
            insn @ (Insn::TableSwitch { .. } | Insn::LookupSwitch { .. }) => {
                if insn.jump_targets().iter().any(|to| standing.contains(to)) {
                    return None;
                }
            }
            _ => {}
        }
    }
    Some((start, jumps))
}

/// Fold every null check whose value the path after it loads again; `None` when a check kotlinc
/// could fold was not proven foldable here, and the method then keeps every check as it was.
pub(super) fn fold(body: &mut Body) -> Option<Folds> {
    let loads: Vec<NodeId> = body
        .instructions()
        .into_iter()
        .filter(|&id| loads_reference(body.insn(id), None))
        .collect();
    let candidates = loads
        .iter()
        .filter(|&&load| candidate(body, load).is_some())
        .count();
    let mut folds = Folds {
        folded: false,
        null_targets: Vec::new(),
    };
    if candidates == 0 {
        return Some(folds);
    }
    // Each candidate may inspect every jump into its target and edit the body. Keep the same
    // per-method ceiling as the temporary-value data-flow analysis rather than allowing a large
    // generated method to turn this pre-pass quadratic without bound.
    if !analysis_within_limit(body.instructions().len(), candidates, 1) {
        return None;
    }
    for load in loads {
        if !body.has(load) {
            continue;
        }
        let Some((slot, jump)) = candidate(body, load) else {
            continue;
        };
        if body.labelled(jump) {
            continue;
        }
        let Some((op, target)) = null_jump(body.insn(jump)) else {
            continue;
        };
        let Some((start, jumps)) = only_jumps_to(body, target) else {
            continue;
        };
        if op == IFNONNULL {
            // The jump is the target's only predecessor, and the target reloads the value first
            // thing: no line number or local range stands there.
            if jumps != [jump] || body.marked(start) {
                continue;
            }
            let Some((reload, swap_after)) = reload(body, Some(start), slot) else {
                continue;
            };
            body.insert_before(jump, Insn::Op(DUP));
            body.insert_after(jump, Insn::Op(POP));
            if let Some(under) = swap_after {
                body.insert_after(under, Insn::Op(SWAP));
            }
            body.remove(reload);
            folds.folded = true;
            continue;
        }
        // `ifnull`: every jump to the target's label checks a local and reloads it on the
        // fall-through. Of the labels standing there, one before it is a predecessor that is not
        // such a jump; one after it is not a predecessor at all, and lands after the `pop`.
        let standing = body.labels_before(start);
        let rank = |label: &LabelId| standing.iter().position(|other| other == label);
        let Some(own) = rank(&target) else {
            continue;
        };
        let mut to_own = Vec::new();
        let mut foreign = false;
        for &other in &jumps {
            let Insn::Jump { target: label, .. } = body.insn(other) else {
                unreachable!("only_jumps_to returns jumps");
            };
            match rank(label) {
                Some(rank) if rank == own => to_own.push(other),
                Some(rank) if rank > own => {}
                _ => foreign = true,
            }
        }
        if foreign {
            continue;
        }
        let mut parts = Vec::with_capacity(to_own.len());
        for &other in &to_own {
            let checked = body.prev_insn(other).filter(|_| {
                matches!(null_jump(body.insn(other)), Some((IFNULL, _))) && !body.labelled(other)
            });
            let Some(checked) = checked else {
                break;
            };
            let Some(VarOp::Load(Kind::Reference, checked_slot)) = var_op(body.insn(checked))
            else {
                break;
            };
            let Some(next) = body.next_insn(other).filter(|&next| !body.labelled(next)) else {
                break;
            };
            let Some((reload, swap_after)) = reload(body, Some(next), checked_slot) else {
                break;
            };
            parts.push((other, reload, swap_after));
        }
        if parts.len() != to_own.len() {
            continue;
        }
        for &(other, reload, swap_after) in &parts {
            body.insert_before(other, Insn::Op(DUP));
            if let Some(under) = swap_after {
                body.insert_after(under, Insn::Op(SWAP));
            }
            body.remove(reload);
        }
        let pop = body.insert_before(start, Insn::Op(POP));
        folds.folded = true;
        folds.null_targets.push(NullTarget { pop, label: target });
    }
    // The temporary and `nop` cleanups must not apply only half of kotlinc's coupled safe-call
    // transformation. If a candidate kotlinc's matcher could still rewrite was not proven foldable,
    // keep the method as it was. One its matcher cannot match stays as it is in kotlinc too.
    let remaining = body.instructions();
    if remaining
        .iter()
        .any(|&load| candidate(body, load).is_some() && kotlinc_could_rewrite(body, load))
    {
        return None;
    }
    Some(folds)
}

/// Whether kotlinc's `simplifyKnownSafeCallPatterns` could rewrite the `aload v; ifnull`/`ifnonnull`
/// pair at `load`. Its matcher walks raw instruction successors: a kept label between the load and
/// the jump ends the match, and so does one in front of the reload it expects — `aload v`, or
/// `aload x`/a one-word `getstatic` followed by `aload v`. `ifnonnull` also needs to be its target's
/// only predecessor; `ifnull` rewrites a target only when every jump to it is such a part. A pair
/// failing those tests is one kotlinc leaves unchanged.
///
/// A target something besides jumps reaches has another predecessor in kotlinc too — a
/// fall-through (a return's dead `nop` included), a switch or a handler edge — which neither
/// rewrite accepts. Only a protected range's bound is reached by no predecessor of its own, so
/// one at the target's own label answers that kotlinc could, keeping the caller conservative; one
/// that stood past an instruction the cleanups removed since is at another label in kotlinc.
fn kotlinc_could_rewrite(body: &Body, load: NodeId) -> bool {
    let part = |jump: NodeId| {
        let Some(checked) = body.prev_insn(jump) else {
            return false;
        };
        let Some(VarOp::Load(Kind::Reference, slot)) = var_op(body.insn(checked)) else {
            return false;
        };
        if body.labelled(jump) {
            return false;
        }
        match null_jump(body.insn(jump)) {
            Some((IFNONNULL, target)) => {
                let start = body.insn_at(target);
                !start.is_some_and(|start| body.marked(start))
                    && reload(body, start, slot).is_some()
            }
            Some((IFNULL, _)) => body
                .next_insn(jump)
                .filter(|&next| !body.labelled(next))
                .is_some_and(|next| reload(body, Some(next), slot).is_some()),
            _ => false,
        }
    };
    let Some((_, jump)) = candidate(body, load) else {
        return false;
    };
    let Some((op, target)) = null_jump(body.insn(jump)) else {
        return false;
    };
    let Some((_, jumps)) = only_jumps_to(body, target) else {
        return body.protected_bound_at(target);
    };
    if op == IFNONNULL {
        jumps == [jump] && part(jump)
    } else {
        jumps.iter().all(|&jump| part(jump))
    }
}

/// Stand what describes each `ifnull` target's instruction after the `pop` put in front of it:
/// the labels after the folded jumps' own, and the line numbers and local-range bounds standing
/// there, under a new label. Returns the labels now after a `pop`, which the later passes pin.
pub(super) fn place_after_pops(
    body: &mut Body,
    method: &mut MethodNode,
    targets: &[NullTarget],
) -> BTreeSet<LabelId> {
    let mut pinned = BTreeSet::new();
    for target in targets {
        let run = body.run_before(target.pop);
        let own = run
            .iter()
            .position(|&at| *body.list.node(at) == Node::Label(target.label))
            .expect("the folded jumps' label stands in front of its pop");
        let mut late: Vec<Node> = Vec::new();
        let mut lines = Vec::new();
        let mut standing = Vec::new();
        for (k, &at) in run.iter().enumerate() {
            match body.list.node(at).clone() {
                Node::Label(label) => {
                    standing.push(label);
                    if k > own {
                        pinned.insert(label);
                        late.push(Node::Label(label));
                        body.list.remove(at);
                    }
                }
                Node::Line { line, .. } => {
                    lines.push(line);
                    body.list.remove(at);
                }
                Node::Insn(_) => unreachable!("a run holds labels and line numbers"),
            }
        }
        let bounded = method
            .local_variables
            .iter()
            .any(|local| standing.contains(&local.start) || standing.contains(&local.end));
        if !lines.is_empty() || bounded {
            let debug = method.new_label();
            pinned.insert(debug);
            late.push(Node::Label(debug));
            late.extend(
                lines
                    .into_iter()
                    .map(|line| Node::Line { line, start: debug }),
            );
            for local in &mut method.local_variables {
                if standing.contains(&local.start) {
                    local.start = debug;
                }
                if standing.contains(&local.end) {
                    local.end = debug;
                }
            }
        }
        body.list.insert_after(Some(target.pop), late);
    }
    pinned
}
