//! kotlinc's `StackPeepholeOptimizationsTransformer`: local rewrites of a value pushed only to be
//! discarded or reordered, repeated until none applies. It runs after the temporaries pass.
//!
//! Each rule looks back from an instruction to the previous meaningful one — skipping `nop`s, line
//! numbers and labels no branch reaches, stopping at a label a branch or handler reaches:
//!
//! - a pure push or `dup`, then `pop`: both become `nop`; `dup_x1`, then `pop`: `swap`;
//! - a two-word pure push or `dup2`, then `pop2`: both become `nop`; so do two one-word pushes
//!   (or `dup`s) followed by `pop2`;
//! - two one-word pure pushes, then `swap`: the pushes trade places and the `swap` goes;
//! - `iconst_0`/`iconst_1`, then `i2l`: `lconst_0`/`lconst_1`.
//!
//! A pure push is a constant, a local load, a one-word or two-word `ldc`, or `Unit.INSTANCE`. The
//! `nop`s this leaves go the way kotlinc's `nop` cleanup takes them: only the first `nop` of a
//! debug range holding nothing else, and one opening a protected range, stays.

use std::collections::BTreeSet;

use super::redundant_gotos::{self, Tables};
use super::temporaries::Placement;
use crate::jvm::inline::{BranchTarget, Insn};

const NOP: u8 = 0x00;
const POP: u8 = 0x57;
const POP2: u8 = 0x58;
const DUP: u8 = 0x59;
const DUP_X1: u8 = 0x5a;
const DUP2: u8 = 0x5c;
const SWAP: u8 = 0x5f;
const I2L: u8 = 0x85;
const ICONST_0: u8 = 0x03;
const ICONST_1: u8 = 0x04;
const LCONST_0: u8 = 0x09;
const LCONST_1: u8 = 0x0a;
const LDC2_W: u8 = 0x14;
const GETSTATIC: u8 = 0xb2;

/// What the rules read from the constant pool.
pub(crate) struct Pool<'a> {
    /// Whether this `getstatic` reads `kotlin/Unit.INSTANCE`.
    pub unit_instance: &'a dyn Fn(u16) -> bool,
}

fn plain(op: u8) -> Insn {
    Insn::Plain {
        op,
        operands: Vec::new(),
    }
}

fn opcode(insn: &Insn) -> Option<u8> {
    match insn {
        Insn::Plain { op, .. } => Some(*op),
        _ => None,
    }
}

/// The opcode a local load has in kotlinc's tree form (`aload_1` is `aload 1`), `None` otherwise.
fn load_kind(insn: &Insn) -> Option<u8> {
    let Insn::Plain { op, operands } = insn else {
        return None;
    };
    match *op {
        0x15..=0x19 => Some(*op),
        0x1a..=0x2d => Some(0x15 + (*op - 0x1a) / 4),
        0xc4 => operands
            .first()
            .copied()
            .filter(|inner| (0x15..=0x19).contains(inner)),
        _ => None,
    }
}

fn pure_push_of_size_1(insn: &Insn, pool: &Pool) -> bool {
    let Insn::Plain { op, operands } = insn else {
        return false;
    };
    match *op {
        // kotlinc's range: `aconst_null` through `fconst_2`.
        0x01..=0x0d => true,
        // `bipush`, `sipush`, `ldc`, `ldc_w`; `ldc2_w` pushes two words.
        0x10..=0x13 => true,
        GETSTATIC => operands
            .get(..2)
            .is_some_and(|field| (pool.unit_instance)(u16::from_be_bytes([field[0], field[1]]))),
        _ => matches!(load_kind(insn), Some(0x15 | 0x17 | 0x19)),
    }
}

fn pure_push_of_size_2(insn: &Insn) -> bool {
    matches!(
        opcode(insn),
        Some(LDC2_W | LCONST_0 | LCONST_1 | 0x0e | 0x0f)
    ) || matches!(load_kind(insn), Some(0x16 | 0x18))
}

fn eliminated_by_pop(insn: &Insn, pool: &Pool) -> bool {
    pure_push_of_size_1(insn, pool) || opcode(insn) == Some(DUP)
}

fn eliminated_by_pop2(insn: &Insn) -> bool {
    pure_push_of_size_2(insn) || opcode(insn) == Some(DUP2)
}

/// Original indices a branch, switch or handler reaches: kotlinc's merge nodes.
fn merge_points(nodes: &[(Insn, Placement)], handlers: &[usize]) -> BTreeSet<usize> {
    let mut merges: BTreeSet<usize> = handlers.iter().copied().collect();
    for (insn, _) in nodes {
        match insn {
            Insn::Branch {
                target: BranchTarget::Internal(to),
                ..
            }
            | Insn::BranchW {
                target: BranchTarget::Internal(to),
                ..
            } => {
                merges.insert(*to);
            }
            Insn::TableSwitch {
                default, targets, ..
            } => {
                merges.insert(*default);
                merges.extend(targets.iter().copied());
            }
            Insn::LookupSwitch { default, pairs } => {
                merges.insert(*default);
                merges.extend(pairs.iter().map(|&(_, to)| to));
            }
            _ => {}
        }
    }
    merges
}

/// The previous meaningful node before `at`: `nop`s and labels no branch reaches are skipped; a
/// label a branch or handler reaches stops the search.
fn previous_meaningful(
    nodes: &[(Insn, Placement)],
    merges: &BTreeSet<usize>,
    at: usize,
) -> Option<usize> {
    let mut next = at;
    loop {
        let before = next.checked_sub(1)?;
        let (from, to) = (nodes[before].1.group(), nodes[next].1.group());
        if from != to && merges.range(from + 1..=to).next().is_some() {
            return None;
        }
        if opcode(&nodes[before].0) != Some(NOP) {
            return Some(before);
        }
        next = before;
    }
}

/// Apply kotlinc's peephole rules to `nodes`; `true` when anything changed.
pub(crate) fn optimize(
    nodes: &mut Vec<(Insn, Placement)>,
    tables: &Tables,
    handler_entries: &[usize],
    pool: &Pool,
) -> bool {
    let merges = merge_points(nodes, handler_entries);
    let mut made_nops: BTreeSet<usize> = BTreeSet::new();
    let mut to_nop = |nodes: &mut Vec<(Insn, Placement)>, at: usize| {
        nodes[at].0 = plain(NOP);
        made_nops.insert(at);
    };
    let mut changed = false;
    loop {
        let mut again = false;
        for at in 0..nodes.len() {
            let Some(op) = opcode(&nodes[at].0) else {
                continue;
            };
            match op {
                POP => {
                    let Some(pushed) = previous_meaningful(nodes, &merges, at) else {
                        continue;
                    };
                    if eliminated_by_pop(&nodes[pushed].0, pool) {
                        to_nop(nodes, pushed);
                        to_nop(nodes, at);
                        again = true;
                    } else if opcode(&nodes[pushed].0) == Some(DUP_X1) {
                        to_nop(nodes, pushed);
                        nodes[at].0 = plain(SWAP);
                        again = true;
                    }
                }
                SWAP => {
                    let Some(second) = previous_meaningful(nodes, &merges, at) else {
                        continue;
                    };
                    let Some(first) = previous_meaningful(nodes, &merges, second) else {
                        continue;
                    };
                    if pure_push_of_size_1(&nodes[first].0, pool)
                        && pure_push_of_size_1(&nodes[second].0, pool)
                    {
                        to_nop(nodes, at);
                        let (a, b) = (nodes[first].0.clone(), nodes[second].0.clone());
                        nodes[first].0 = b;
                        nodes[second].0 = a;
                        again = true;
                    }
                }
                I2L => {
                    let Some(constant) = previous_meaningful(nodes, &merges, at) else {
                        continue;
                    };
                    let long = match opcode(&nodes[constant].0) {
                        Some(ICONST_0) => LCONST_0,
                        Some(ICONST_1) => LCONST_1,
                        _ => continue,
                    };
                    to_nop(nodes, constant);
                    nodes[at].0 = plain(long);
                    again = true;
                }
                POP2 => {
                    let Some(pushed) = previous_meaningful(nodes, &merges, at) else {
                        continue;
                    };
                    if eliminated_by_pop2(&nodes[pushed].0) {
                        to_nop(nodes, pushed);
                        to_nop(nodes, at);
                        again = true;
                        continue;
                    }
                    let Some(below) = previous_meaningful(nodes, &merges, pushed) else {
                        continue;
                    };
                    if eliminated_by_pop(&nodes[pushed].0, pool)
                        && eliminated_by_pop(&nodes[below].0, pool)
                    {
                        to_nop(nodes, below);
                        to_nop(nodes, pushed);
                        to_nop(nodes, at);
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
    let required = redundant_gotos::required_nops(nodes, tables);
    let mut removed: Vec<usize> = made_nops
        .into_iter()
        .filter(|at| opcode(&nodes[*at].0) == Some(NOP) && !required.contains(at))
        .collect();
    removed.sort_unstable();
    for &at in removed.iter().rev() {
        nodes.remove(at);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(op: u8) -> Insn {
        plain(op)
    }

    fn with(op: u8, operands: &[u8]) -> Insn {
        Insn::Plain {
            op,
            operands: operands.to_vec(),
        }
    }

    fn run(insns: &[Insn], lines: &[usize]) -> Option<Vec<Insn>> {
        let mut nodes: Vec<(Insn, Placement)> = insns
            .iter()
            .enumerate()
            .map(|(index, insn)| (insn.clone(), Placement::Original(index)))
            .collect();
        let mut line = vec![false; insns.len() + 1];
        for &index in lines {
            line[index] = true;
        }
        let bounds = vec![false; insns.len() + 1];
        let tables = Tables {
            lines: &line,
            variable_bounds: &bounds,
            protected_starts: &[],
            late_branch: &|_| false,
        };
        let pool = Pool {
            unit_instance: &|field| field == 5,
        };
        optimize(&mut nodes, &tables, &[], &pool)
            .then(|| nodes.into_iter().map(|(insn, _)| insn).collect())
    }

    const INVOKE: u8 = 0xb6;
    const RETURN: u8 = 0xb1;

    #[test]
    fn a_discarded_unit_instance_disappears() {
        // `…; invokevirtual; pop; getstatic Unit.INSTANCE; pop; return`
        let call = with(INVOKE, &[0, 3]);
        let insns = [
            op(0x2a),
            call.clone(),
            op(POP),
            with(GETSTATIC, &[0, 5]),
            op(POP),
            op(RETURN),
        ];
        assert_eq!(
            run(&insns, &[]),
            Some(vec![op(0x2a), call, op(POP), op(RETURN)])
        );
    }

    #[test]
    fn a_push_and_pop_across_a_line_number_keep_a_nop_for_the_line() {
        // The pair is the second line's only code: kotlinc keeps a `nop` for that line.
        let call = with(INVOKE, &[0, 3]);
        let insns = [
            op(0x2a),
            call.clone(),
            with(GETSTATIC, &[0, 5]),
            op(POP),
            op(RETURN),
        ];
        assert_eq!(
            run(&insns, &[0, 2, 4]),
            Some(vec![op(0x2a), call, op(NOP), op(RETURN)])
        );
    }

    #[test]
    fn a_branch_target_between_push_and_pop_keeps_both() {
        // 0 iload_0; 1 ifeq 3; 2 aconst_null; 3 pop … — `pop` at a merge point.
        let insns = [
            op(0x1a),
            Insn::Branch {
                op: 0x99,
                target: BranchTarget::Internal(3),
            },
            op(0x01),
            op(POP),
            op(RETURN),
        ];
        assert_eq!(run(&insns, &[]), None);
    }

    #[test]
    fn two_pure_pushes_swapped_trade_places() {
        let insns = [op(0x2a), op(0x2b), op(SWAP), op(0xb5)];
        assert_eq!(run(&insns, &[]), Some(vec![op(0x2b), op(0x2a), op(0xb5)]));
    }
}
