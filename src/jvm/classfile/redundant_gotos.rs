//! kotlinc's redundant-`goto` removal and the `nop` cleanup that follows it, over a rewritten body.
//!
//! kotlinc's `RedundantGotoMethodTransformer` walks the instruction list backwards. Labels and `nop`s
//! are transparent; a line number or any other instruction ends the run of labels in front of it.
//! A `goto` whose target is one of the labels right after it does nothing and becomes a `nop`; and
//! every jump to a label that leads straight into a `goto` is threaded to where that `goto` (and
//! any `goto` it leads into in turn) finally goes.
//!
//! `RedundantNopsCleanupMethodTransformer` then removes the `nop`s, except the first `nop` of a debug
//! range — from a line number or a local variable's range boundary up to the next line number —
//! that holds no other instruction, and a `nop` that is the first instruction of a protected range.
//! Only the `nop`s this pass made are cleaned up: every other `nop` krusty writes is already one
//! kotlinc keeps.

use std::collections::{BTreeMap, BTreeSet};

use super::temporaries::Placement;
use crate::jvm::inline::{BranchTarget, Insn};

const GOTO: u8 = 0xa7;
const NOP: u8 = 0x00;

/// The tables the passes read, by original instruction index (length `insns + 1`).
pub(crate) struct Tables<'a> {
    /// A `LineNumberTable` entry begins here.
    pub lines: &'a [bool],
    /// A local variable's range begins or ends here.
    pub variable_bounds: &'a [bool],
    /// Where each protected range begins.
    pub protected_starts: &'a [usize],
}

fn goto_target(insn: &Insn) -> Option<usize> {
    match insn {
        Insn::Branch {
            op: GOTO,
            target: BranchTarget::Internal(to),
        } => Some(*to),
        _ => None,
    }
}

fn is_nop(insn: &Insn) -> bool {
    matches!(insn, Insn::Plain { op: NOP, operands } if operands.is_empty())
}

/// Remove kotlinc's redundant `goto`s from `nodes` and thread jumps through `goto`s; `true` when
/// anything changed.
pub(crate) fn remove(nodes: &mut Vec<(Insn, Placement)>, tables: &Tables) -> bool {
    let groups = tables.lines.len();
    // Node positions per group, in order.
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); groups];
    for (at, (_, placement)) in nodes.iter().enumerate() {
        members[placement.group().min(groups - 1)].push(at);
    }
    // Backwards: `run` holds the groups whose labels stand in front of the next instruction that
    // is not a `nop`; `next_goto` is that instruction when it is a `goto`.
    let mut run: BTreeSet<usize> = BTreeSet::new();
    let mut next_goto: Option<usize> = None;
    let mut leads_to: BTreeMap<usize, usize> = BTreeMap::new();
    let mut redundant: Vec<usize> = Vec::new();
    for group in (0..groups).rev() {
        for &at in members[group].iter().rev() {
            let insn = &nodes[at].0;
            if let Some(target) = goto_target(insn) {
                next_goto = Some(at);
                if run.contains(&target) {
                    redundant.push(at);
                } else {
                    run.clear();
                }
            } else if !is_nop(insn) {
                run.clear();
                next_goto = None;
            }
        }
        // The group's label follows its line number backwards: `label; LINENUMBER; insn`.
        if tables.lines[group] {
            run.clear();
            next_goto = None;
        }
        run.insert(group);
        if let Some(goto) = next_goto {
            leads_to.insert(group, goto);
        }
    }
    let mut changed = false;
    // Thread every jump through the `goto`s its target leads into.
    let final_target = |start: usize| -> Option<usize> {
        let mut seen = BTreeSet::new();
        let mut goto = *leads_to.get(&start)?;
        loop {
            if !seen.insert(goto) {
                return None;
            }
            let to = goto_target(&nodes[goto].0)?;
            match leads_to.get(&to) {
                Some(&next) => goto = next,
                None => return Some(to),
            }
        }
    };
    let retargets: Vec<(usize, usize)> = nodes
        .iter()
        .enumerate()
        .filter_map(|(at, (insn, _))| match insn {
            Insn::Branch {
                target: BranchTarget::Internal(to),
                ..
            }
            | Insn::BranchW {
                target: BranchTarget::Internal(to),
                ..
            } => {
                let last = final_target(*to)?;
                (last != *to).then_some((at, last))
            }
            _ => None,
        })
        .collect();
    for (at, to) in retargets {
        if let Insn::Branch { target, .. } | Insn::BranchW { target, .. } = &mut nodes[at].0 {
            *target = BranchTarget::Internal(to);
            changed = true;
        }
    }
    if redundant.is_empty() {
        return changed;
    }
    for &at in &redundant {
        nodes[at].0 = Insn::Plain {
            op: NOP,
            operands: Vec::new(),
        };
    }
    let required = required_nops(nodes, tables);
    let mut removed: Vec<usize> = redundant
        .into_iter()
        .filter(|at| !required.contains(at))
        .collect();
    removed.sort_unstable();
    for &at in removed.iter().rev() {
        nodes.remove(at);
    }
    true
}

/// The `nop`s kotlinc's cleanup keeps, by node position.
fn required_nops(nodes: &[(Insn, Placement)], tables: &Tables) -> BTreeSet<usize> {
    let first_at_or_after = |group: usize| {
        nodes
            .iter()
            .position(|(_, placement)| placement.group() >= group)
    };
    let mut required = BTreeSet::new();
    for &start in tables.protected_starts {
        if let Some(at) = first_at_or_after(start).filter(|&at| is_nop(&nodes[at].0)) {
            required.insert(at);
        }
    }
    // Debug points in list order; at one index a variable bound's label comes before the line.
    let mut points: Vec<(usize, bool)> = Vec::new();
    for group in 0..tables.lines.len() {
        if tables.variable_bounds[group] {
            points.push((group, false));
        }
        if tables.lines[group] {
            points.push((group, true));
        }
    }
    for pair in points.windows(2) {
        let [(from, _), (to, true)] = pair else {
            continue;
        };
        let range = nodes
            .iter()
            .enumerate()
            .filter(|(_, (_, placement))| (*from..*to).contains(&placement.group()));
        let mut first_nop = None;
        let mut only_nops = true;
        for (at, (insn, _)) in range {
            if is_nop(insn) {
                first_nop.get_or_insert(at);
            } else {
                only_nops = false;
                break;
            }
        }
        if let (true, Some(at)) = (only_nops, first_nop) {
            required.insert(at);
        }
    }
    required
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(op: u8) -> Insn {
        Insn::Plain {
            op,
            operands: Vec::new(),
        }
    }

    fn branch(op: u8, to: usize) -> Insn {
        Insn::Branch {
            op,
            target: BranchTarget::Internal(to),
        }
    }

    fn run(insns: &[Insn], lines: &[usize], bounds: &[usize]) -> Option<Vec<Insn>> {
        let mut nodes: Vec<(Insn, Placement)> = insns
            .iter()
            .enumerate()
            .map(|(index, insn)| (insn.clone(), Placement::Original(index)))
            .collect();
        let mut line = vec![false; insns.len() + 1];
        for &index in lines {
            line[index] = true;
        }
        let mut bound = vec![false; insns.len() + 1];
        for &index in bounds {
            bound[index] = true;
        }
        let tables = Tables {
            lines: &line,
            variable_bounds: &bound,
            protected_starts: &[],
        };
        remove(&mut nodes, &tables).then(|| nodes.into_iter().map(|(insn, _)| insn).collect())
    }

    const IFNONNULL: u8 = 0xc7;
    const ARETURN: u8 = 0xb0;

    #[test]
    fn a_goto_to_the_next_instruction_is_removed() {
        // `s ?: "d"` after the null-check fold: 0 aload_0; 1 dup; 2 ifnonnull 6; 3 pop;
        // 4 aconst_null; 5 goto 6; 6 areturn.
        let insns = [
            op(0x2a),
            op(0x59),
            branch(IFNONNULL, 6),
            op(0x57),
            op(0x01),
            branch(GOTO, 6),
            op(ARETURN),
        ];
        assert_eq!(
            run(&insns, &[0], &[0, 7]),
            Some(vec![
                op(0x2a),
                op(0x59),
                branch(IFNONNULL, 6),
                op(0x57),
                op(0x01),
                op(ARETURN),
            ])
        );
    }

    #[test]
    fn a_line_number_at_a_label_between_keeps_the_goto() {
        // 0 iconst_0; 1 goto 3; 2 (a line starts here) nop; 3 ireturn — the line between ends the run.
        let insns = [op(0x03), branch(GOTO, 3), op(NOP), op(0xac)];
        assert_eq!(run(&insns, &[0, 2], &[]), None);
    }

    #[test]
    fn a_goto_alone_on_its_line_leaves_a_nop() {
        // 0 iconst_0; 1 istore_0; 2 (line) goto 3; 3 (line) iload_0; 4 ireturn: the goto is the
        // only instruction of its line, so kotlinc keeps a `nop` there.
        let insns = [op(0x03), op(0x3b), branch(GOTO, 3), op(0x1a), op(0xac)];
        assert_eq!(
            run(&insns, &[0, 2, 3], &[]),
            Some(vec![op(0x03), op(0x3b), op(NOP), op(0x1a), op(0xac)])
        );
    }

    #[test]
    fn a_jump_into_a_goto_goes_where_the_goto_goes() {
        // 0 iload_0; 1 ifeq 4; 2 iconst_1; 3 ireturn; 4 goto 6; 5 iconst_2; 6 iconst_0; 7 ireturn.
        let insns = [
            op(0x1a),
            branch(0x99, 4),
            op(0x04),
            op(0xac),
            branch(GOTO, 6),
            op(0x05),
            op(0x03),
            op(0xac),
        ];
        assert_eq!(
            run(&insns, &[], &[]),
            Some(vec![
                op(0x1a),
                branch(0x99, 6),
                op(0x04),
                op(0xac),
                branch(GOTO, 6),
                op(0x05),
                op(0x03),
                op(0xac),
            ])
        );
    }
}
