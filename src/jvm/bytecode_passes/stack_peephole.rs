//! kotlinc's `StackPeepholeOptimizationsTransformer`: local rewrites of a value pushed only to be
//! discarded or reordered, repeated until none applies. It runs after the temporaries pass.
//!
//! Each rule looks back from an instruction to the previous meaningful one, skipping `nop`s, line
//! numbers and labels no branch reaches, and stopping at a label a branch or handler reaches:
//!
//! - a pure push or `dup`, then `pop`: both become `nop`; `dup_x1`, then `pop`: `swap`;
//! - a two-word pure push or `dup2`, then `pop2`: both become `nop`; so do two one-word pushes
//!   (or `dup`s) followed by `pop2`;
//! - two one-word pure pushes, then `swap`: the pushes trade places and the `swap` goes;
//! - `iconst_0`/`iconst_1`, then `i2l`: `lconst_0`/`lconst_1`;
//! - `Intrinsics.compare(Int, Int)` followed by a zero comparison: compare the inputs directly.
//!
//! A pure push is a constant, a local load, a one-word or two-word `ldc`, or `Unit.INSTANCE`. The
//! `nop`s this leaves go the way kotlinc's `nop` cleanup takes them (see
//! [`super::redundant_gotos::required_nops`]).

use std::collections::BTreeSet;

use super::redundant_gotos::required_nops;
use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

const NOP: u8 = 0x00;
const ICONST_0: u8 = 0x03;
const ICONST_1: u8 = 0x04;
const LCONST_0: u8 = 0x09;
const LCONST_1: u8 = 0x0a;
const ILOAD: u8 = 0x15;
const LLOAD: u8 = 0x16;
const FLOAD: u8 = 0x17;
const DLOAD: u8 = 0x18;
const ALOAD: u8 = 0x19;
const POP: u8 = 0x57;
const POP2: u8 = 0x58;
const DUP: u8 = 0x59;
const DUP_X1: u8 = 0x5a;
const DUP2: u8 = 0x5c;
const SWAP: u8 = 0x5f;
const I2L: u8 = 0x85;
const IFEQ: u8 = 0x99;
const IFLE: u8 = 0x9e;
const IF_ICMPEQ: u8 = 0x9f;
const IF_ICMPLE: u8 = 0xa4;
const GETSTATIC: u8 = 0xb2;
const INVOKESTATIC: u8 = 0xb8;

fn pure_push_of_size_1(insn: &Insn) -> bool {
    match insn {
        // kotlinc's range: `aconst_null` through `fconst_2`.
        Insn::Op(op) => (0x01..=0x0d).contains(op),
        // `bipush`, `sipush`.
        Insn::Int { op, .. } => matches!(*op, 0x10 | 0x11),
        Insn::Ldc(constant) => !constant.is_wide(),
        Insn::Var { op, .. } => matches!(*op, ILOAD | FLOAD | ALOAD),
        Insn::Field {
            op: GETSTATIC,
            owner,
            name,
            desc,
        } => owner == "kotlin/Unit" && name == "INSTANCE" && desc == "Lkotlin/Unit;",
        _ => false,
    }
}

fn pure_push_of_size_2(insn: &Insn) -> bool {
    match insn {
        Insn::Op(op) => matches!(*op, LCONST_0 | LCONST_1 | 0x0e | 0x0f),
        Insn::Ldc(constant) => constant.is_wide(),
        Insn::Var { op, .. } => matches!(*op, LLOAD | DLOAD),
        _ => false,
    }
}

fn eliminated_by_pop(insn: &Insn) -> bool {
    pure_push_of_size_1(insn) || *insn == Insn::Op(DUP)
}

fn eliminated_by_pop2(insn: &Insn) -> bool {
    pure_push_of_size_2(insn) || *insn == Insn::Op(DUP2)
}

fn compare_int(insn: &Insn) -> bool {
    matches!(
        insn,
        Insn::Method { op: INVOKESTATIC, owner, name, desc, .. }
            if owner == "kotlin/jvm/internal/Intrinsics" && name == "compare" && desc == "(II)I"
    )
}

/// The labels a branch, switch or handler reaches: kotlinc's merge nodes.
fn merge_points(method: &MethodNode) -> BTreeSet<LabelId> {
    let mut merges: BTreeSet<LabelId> = method
        .try_catch_blocks
        .iter()
        .map(|block| block.handler)
        .collect();
    merges.extend(method.instructions().flat_map(Insn::jump_targets));
    merges
}

/// The previous meaningful node before `at`: `nop`s, line numbers and labels no branch reaches are
/// skipped; a label a branch or handler reaches stops the search.
fn previous_meaningful(nodes: &[Node], merges: &BTreeSet<LabelId>, at: usize) -> Option<usize> {
    (0..at).rev().find_map(|before| match &nodes[before] {
        Node::Label(label) if merges.contains(label) => Some(None),
        Node::Label(_) | Node::Line { .. } | Node::Insn(Insn::Op(NOP)) => None,
        Node::Insn(_) => Some(Some(before)),
    })?
}

fn insn(nodes: &[Node], at: usize) -> &Insn {
    match &nodes[at] {
        Node::Insn(insn) => insn,
        _ => unreachable!("previous_meaningful returns only instructions"),
    }
}

/// Apply kotlinc's peephole rules to `method`; `true` when anything changed.
pub(crate) fn optimize(method: &mut MethodNode) -> bool {
    let merges = merge_points(method);
    let nodes = &mut method.nodes;
    let mut made_nops: BTreeSet<usize> = BTreeSet::new();
    let mut set = |nodes: &mut Vec<Node>, at: usize, insn: Insn| {
        if insn == Insn::Op(NOP) {
            made_nops.insert(at);
        }
        nodes[at] = Node::Insn(insn);
    };
    let mut changed = false;
    loop {
        let mut again = false;
        for at in 0..nodes.len() {
            let Node::Insn(current) = &nodes[at] else {
                continue;
            };
            match *current {
                Insn::Jump {
                    op: op @ IFEQ..=IFLE,
                    target,
                } => {
                    let Some(compared) = previous_meaningful(nodes, &merges, at) else {
                        continue;
                    };
                    if compare_int(insn(nodes, compared)) {
                        set(
                            nodes,
                            compared,
                            Insn::Jump {
                                op: op - IFEQ + IF_ICMPEQ,
                                target,
                            },
                        );
                        set(nodes, at, Insn::Op(NOP));
                        again = true;
                    }
                }
                Insn::Jump {
                    op: IF_ICMPEQ..=IF_ICMPLE,
                    ..
                } => {
                    let Some(zero) = previous_meaningful(nodes, &merges, at) else {
                        continue;
                    };
                    if *insn(nodes, zero) != Insn::Op(ICONST_0) {
                        continue;
                    }
                    let Some(compared) = previous_meaningful(nodes, &merges, zero) else {
                        continue;
                    };
                    if compare_int(insn(nodes, compared)) {
                        set(nodes, compared, Insn::Op(NOP));
                        set(nodes, zero, Insn::Op(NOP));
                        again = true;
                    }
                }
                Insn::Op(POP) => {
                    let Some(pushed) = previous_meaningful(nodes, &merges, at) else {
                        continue;
                    };
                    if eliminated_by_pop(insn(nodes, pushed)) {
                        set(nodes, pushed, Insn::Op(NOP));
                        set(nodes, at, Insn::Op(NOP));
                        again = true;
                    } else if *insn(nodes, pushed) == Insn::Op(DUP_X1) {
                        set(nodes, pushed, Insn::Op(SWAP));
                        set(nodes, at, Insn::Op(NOP));
                        again = true;
                    }
                }
                Insn::Op(SWAP) => {
                    let Some(second) = previous_meaningful(nodes, &merges, at) else {
                        continue;
                    };
                    let Some(first) = previous_meaningful(nodes, &merges, second) else {
                        continue;
                    };
                    let (a, b) = (insn(nodes, first).clone(), insn(nodes, second).clone());
                    if pure_push_of_size_1(&a) && pure_push_of_size_1(&b) {
                        set(nodes, at, Insn::Op(NOP));
                        set(nodes, first, b);
                        set(nodes, second, a);
                        again = true;
                    }
                }
                Insn::Op(I2L) => {
                    let Some(constant) = previous_meaningful(nodes, &merges, at) else {
                        continue;
                    };
                    let long = match *insn(nodes, constant) {
                        Insn::Op(ICONST_0) => LCONST_0,
                        Insn::Op(ICONST_1) => LCONST_1,
                        _ => continue,
                    };
                    set(nodes, constant, Insn::Op(long));
                    set(nodes, at, Insn::Op(NOP));
                    again = true;
                }
                Insn::Op(POP2) => {
                    let Some(pushed) = previous_meaningful(nodes, &merges, at) else {
                        continue;
                    };
                    if eliminated_by_pop2(insn(nodes, pushed)) {
                        set(nodes, pushed, Insn::Op(NOP));
                        set(nodes, at, Insn::Op(NOP));
                        again = true;
                        continue;
                    }
                    let Some(below) = previous_meaningful(nodes, &merges, pushed) else {
                        continue;
                    };
                    if eliminated_by_pop(insn(nodes, pushed))
                        && eliminated_by_pop(insn(nodes, below))
                    {
                        set(nodes, below, Insn::Op(NOP));
                        set(nodes, pushed, Insn::Op(NOP));
                        set(nodes, at, Insn::Op(NOP));
                        again = true;
                    }
                }
                _ => {}
            }
        }
        if !again {
            break;
        }
        changed = true;
    }
    if !changed {
        return false;
    }
    let required = required_nops(method);
    let mut removed: Vec<usize> = made_nops
        .into_iter()
        .filter(|&at| method.nodes[at] == Node::Insn(Insn::Op(NOP)) && !required.contains(&at))
        .collect();
    removed.sort_unstable();
    for &at in removed.iter().rev() {
        method.nodes.remove(at);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const RETURN: u8 = 0xb1;
    const PUTFIELD: u8 = 0xb5;

    fn call(name: &str) -> Insn {
        Insn::Method {
            op: 0xb6,
            owner: "A".to_string(),
            name: name.to_string(),
            desc: "()Ljava/lang/Object;".to_string(),
            interface: false,
        }
    }

    fn unit() -> Insn {
        Insn::Field {
            op: GETSTATIC,
            owner: "kotlin/Unit".to_string(),
            name: "INSTANCE".to_string(),
            desc: "Lkotlin/Unit;".to_string(),
        }
    }

    fn compare(owner: &str) -> Insn {
        Insn::Method {
            op: INVOKESTATIC,
            owner: owner.to_string(),
            name: "compare".to_string(),
            desc: "(II)I".to_string(),
            interface: false,
        }
    }

    fn var(op: u8, slot: u16) -> Insn {
        Insn::Var { op, slot }
    }

    /// `insns` with a label before each and one past the end, line numbers starting at `lines`;
    /// a jump's target is given as `Err((op, k))`, to label `k`.
    struct Body {
        method: MethodNode,
        labels: Vec<LabelId>,
    }

    fn body(insns: &[Result<Insn, (u8, usize)>], lines: &[usize]) -> Body {
        let mut method = MethodNode::new(0x0009, "f", "(II)V");
        let labels: Vec<LabelId> = (0..=insns.len()).map(|_| method.new_label()).collect();
        for (k, insn) in insns.iter().enumerate() {
            method.nodes.push(Node::Label(labels[k]));
            if lines.contains(&k) {
                method.nodes.push(Node::Line {
                    line: k as u16 + 1,
                    start: labels[k],
                });
            }
            method.nodes.push(Node::Insn(match insn {
                Ok(insn) => insn.clone(),
                Err((op, to)) => Insn::Jump {
                    op: *op,
                    target: labels[*to],
                },
            }));
        }
        method.nodes.push(Node::Label(labels[insns.len()]));
        Body { method, labels }
    }

    impl Body {
        fn run(mut self) -> Option<Vec<Insn>> {
            optimize(&mut self.method).then(|| self.method.instructions().cloned().collect())
        }

        fn jump(&self, op: u8, to: usize) -> Insn {
            Insn::Jump {
                op,
                target: self.labels[to],
            }
        }
    }

    fn ok(insns: &[Insn]) -> Vec<Result<Insn, (u8, usize)>> {
        insns.iter().cloned().map(Ok).collect()
    }

    #[test]
    fn a_discarded_unit_instance_disappears() {
        let insns = [
            var(ALOAD, 0),
            call("a"),
            Insn::Op(POP),
            unit(),
            Insn::Op(POP),
            Insn::Op(RETURN),
        ];
        assert_eq!(
            body(&ok(&insns), &[]).run(),
            Some(vec![
                var(ALOAD, 0),
                call("a"),
                Insn::Op(POP),
                Insn::Op(RETURN)
            ])
        );
    }

    #[test]
    fn a_push_and_pop_across_a_line_number_keep_a_nop_for_the_line() {
        // The pair is the second line's only code: kotlinc keeps a `nop` for that line.
        let insns = [
            var(ALOAD, 0),
            call("a"),
            unit(),
            Insn::Op(POP),
            Insn::Op(RETURN),
        ];
        assert_eq!(
            body(&ok(&insns), &[0, 2, 4]).run(),
            Some(vec![
                var(ALOAD, 0),
                call("a"),
                Insn::Op(NOP),
                Insn::Op(RETURN)
            ])
        );
    }

    #[test]
    fn a_branch_target_between_push_and_pop_keeps_both() {
        // 0 iload_0; 1 ifeq 3; 2 aconst_null; 3 pop …: `pop` at a merge point.
        let insns = vec![
            Ok(var(ILOAD, 0)),
            Err((IFEQ, 3)),
            Ok(Insn::Op(0x01)),
            Ok(Insn::Op(POP)),
            Ok(Insn::Op(RETURN)),
        ];
        assert_eq!(body(&insns, &[]).run(), None);
    }

    #[test]
    fn two_pure_pushes_swapped_trade_places() {
        let insns = [
            var(ALOAD, 0),
            var(ALOAD, 1),
            Insn::Op(SWAP),
            Insn::Op(PUTFIELD),
        ];
        assert_eq!(
            body(&ok(&insns), &[]).run(),
            Some(vec![var(ALOAD, 1), var(ALOAD, 0), Insn::Op(PUTFIELD)])
        );
    }

    #[test]
    fn dup_x1_and_pop_leave_swap_at_the_dup_and_a_nop_on_the_later_line() {
        // Calls are deliberately not pure pushes: two local loads would let the next peephole
        // exchange them and remove the surviving `swap`, hiding the placement this test owns.
        let insns = [
            call("a"),
            call("b"),
            Insn::Op(DUP_X1),
            Insn::Op(POP),
            Insn::Op(PUTFIELD),
            Insn::Op(RETURN),
        ];
        assert_eq!(
            body(&ok(&insns), &[0, 3, 4]).run(),
            Some(vec![
                call("a"),
                call("b"),
                Insn::Op(SWAP),
                Insn::Op(NOP),
                Insn::Op(PUTFIELD),
                Insn::Op(RETURN),
            ])
        );
    }

    #[test]
    fn int_to_long_leaves_the_long_at_the_constant_and_a_nop_on_the_later_line() {
        let insns = [Insn::Op(ICONST_0), Insn::Op(I2L), Insn::Op(0xad)];
        assert_eq!(
            body(&ok(&insns), &[0, 1, 2]).run(),
            Some(vec![Insn::Op(LCONST_0), Insn::Op(NOP), Insn::Op(0xad)])
        );
    }

    #[test]
    fn compare_int_and_zero_branch_compare_the_inputs_directly() {
        let insns = vec![
            Ok(var(ILOAD, 0)),
            Ok(var(ILOAD, 1)),
            Ok(compare("kotlin/jvm/internal/Intrinsics")),
            Err((IFEQ, 5)),
            Ok(Insn::Op(RETURN)),
            Ok(Insn::Op(RETURN)),
        ];
        let body = body(&insns, &[0, 3, 4]);
        let branch = body.jump(IF_ICMPEQ, 5);
        assert_eq!(
            body.run(),
            Some(vec![
                var(ILOAD, 0),
                var(ILOAD, 1),
                branch,
                Insn::Op(NOP),
                Insn::Op(RETURN),
                Insn::Op(RETURN),
            ])
        );
    }

    #[test]
    fn compare_int_against_an_explicit_zero_drops_both_intermediate_values() {
        let insns = vec![
            Ok(var(ILOAD, 0)),
            Ok(var(ILOAD, 1)),
            Ok(compare("kotlin/jvm/internal/Intrinsics")),
            Ok(Insn::Op(ICONST_0)),
            Err((IF_ICMPLE, 6)),
            Ok(Insn::Op(RETURN)),
            Ok(Insn::Op(RETURN)),
        ];
        let body = body(&insns, &[]);
        let branch = body.jump(IF_ICMPLE, 6);
        assert_eq!(
            body.run(),
            Some(vec![
                var(ILOAD, 0),
                var(ILOAD, 1),
                branch,
                Insn::Op(RETURN),
                Insn::Op(RETURN),
            ])
        );
    }

    #[test]
    fn same_shaped_non_intrinsic_compare_is_not_rewritten() {
        let insns = vec![Ok(compare("A")), Err((IFEQ, 2)), Ok(Insn::Op(RETURN))];
        assert_eq!(body(&insns, &[]).run(), None);
    }
}
