//! kotlinc's final `DeadCodeEliminationMethodTransformer`, over a rewritten body.
//!
//! After its optimization passes kotlinc always runs this transformer once more, so code a pass left
//! unreachable is gone from the class it writes: a `goto` whose jumps the redundant-`goto` pass
//! threaded past it, for one. `InstructionLivenessAnalyzer` walks the method from its first
//! instruction, following fall-through (everything but a `goto`, a switch, a return and `athrow`),
//! jump and switch targets, and, from every live instruction inside a protected range, that range's
//! handler. Every instruction it does not reach is removed.
//!
//! A line number is removed when, scanning forward past labels and past line numbers of the same
//! line, the scan reaches the end of the method or a different line having passed only dead
//! instructions; the first live instruction keeps it, and so does a different line reached with no
//! instruction in between. `removeEmptyCatchBlocks` then drops every protected range left with no
//! instruction, and `prepareForEmitting` every local variable whose range the removal empties.
//!
//! The same transformer then renumbers the local slots (see [`super::local_slots`]).

use super::bytecode_analysis::Handler;
use super::temporaries::Placement;
use crate::jvm::inline::{BranchTarget, Insn};

const GOTO: u8 = 0xa7;
const JSR: u8 = 0xa8;
const RET: u8 = 0xa9;
const GOTO_W: u8 = 0xc8;
const JSR_W: u8 = 0xc9;
const WIDE: u8 = 0xc4;
const ATHROW: u8 = 0xbf;

/// What the analysis reads beside the rewritten nodes, by original instruction index.
pub(crate) struct Flow<'a> {
    /// The method's protected ranges, in exception-table order.
    pub handlers: &'a [Handler],
    /// Whether the branch at an original index jumps to a label standing after the instructions a
    /// rule inserted in front of its target (see [`super::temporaries::Rewrite::late_labels`]).
    pub late_branch: &'a dyn Fn(usize) -> bool,
    /// `LineNumberTable` entries as `(original index, line)`, in table order.
    pub lines: &'a [(usize, u16)],
    /// Whether the line numbers at an original index stand after the instructions inserted in
    /// front of it rather than before them.
    pub line_after_inserted: &'a dyn Fn(usize) -> bool,
    /// Each `LocalVariableTable` entry's range as `[start, end)` original indices, in table order;
    /// `None` for an entry outside the code.
    pub locals: &'a [Option<(usize, usize)>],
}

/// What the removal took with the dead instructions.
pub(crate) struct Elimination {
    /// Per `LineNumberTable` entry, whether it was removed.
    pub removed_lines: Vec<bool>,
    /// Per protected range, whether it was left empty and removed.
    pub removed_handlers: Vec<bool>,
    /// Per `LocalVariableTable` entry, whether its range lost its last instruction.
    pub removed_locals: Vec<bool>,
}

/// Where a label bound at each original index stands among the nodes: in front of the group, and
/// after the instructions inserted in front of it (a late label).
struct Positions {
    early: Vec<usize>,
    late: Vec<usize>,
}

impl Positions {
    fn of(nodes: &[(Insn, Placement)], groups: usize) -> Positions {
        let mut early = vec![nodes.len(); groups];
        let mut late = vec![nodes.len(); groups];
        let mut next = 0;
        let mut next_late = 0;
        for k in 0..groups {
            while next < nodes.len() && nodes[next].1.group() < k {
                next += 1;
            }
            early[k] = next;
            next_late = next_late.max(next);
            while next_late < nodes.len()
                && (nodes[next_late].1.group() < k || nodes[next_late].1 == Placement::Before(k))
            {
                next_late += 1;
            }
            late[k] = next_late;
        }
        Positions { early, late }
    }
}

/// A branch's target as `(original index, late)`.
fn branch_target(node: &(Insn, Placement), flow: &Flow) -> Option<(usize, bool)> {
    let (Insn::Branch {
        target: BranchTarget::Internal(to),
        ..
    }
    | Insn::BranchW {
        target: BranchTarget::Internal(to),
        ..
    }) = &node.0
    else {
        return None;
    };
    let late = matches!(node.1, Placement::Original(index) if (flow.late_branch)(index));
    Some((*to, late))
}

fn switch_targets(insn: &Insn) -> Vec<usize> {
    match insn {
        Insn::TableSwitch {
            default, targets, ..
        } => std::iter::once(*default)
            .chain(targets.iter().copied())
            .collect(),
        Insn::LookupSwitch { default, pairs } => std::iter::once(*default)
            .chain(pairs.iter().map(|&(_, to)| to))
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether control can fall through past `insn`.
fn falls_through(insn: &Insn) -> bool {
    match insn {
        Insn::Branch { op, .. } | Insn::BranchW { op, .. } => !matches!(*op, GOTO | GOTO_W),
        Insn::Plain { op, .. } => !matches!(*op, 0xac..=0xb1 | ATHROW),
        Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => false,
    }
}

/// A subroutine (`jsr`/`ret`), which kotlinc never emits and this analysis does not model.
fn is_subroutine(insn: &Insn) -> bool {
    match insn {
        Insn::Branch { op, .. } | Insn::BranchW { op, .. } => matches!(*op, JSR | JSR_W),
        Insn::Plain { op: RET, .. } => true,
        Insn::Plain { op: WIDE, operands } => operands.first() == Some(&RET),
        _ => false,
    }
}

/// `true` at every node the method's entry reaches, and per handler whether it is reached.
fn liveness(
    nodes: &[(Insn, Placement)],
    flow: &Flow,
    at: &Positions,
) -> Option<(Vec<bool>, Vec<bool>)> {
    let len = nodes.len();
    let early = |k: usize| at.early.get(k).copied();
    let ranges: Vec<(usize, usize, usize)> = flow
        .handlers
        .iter()
        .map(|handler| {
            Some((
                early(handler.start)?,
                early(handler.end)?,
                early(handler.handler)?,
            ))
        })
        .collect::<Option<_>>()?;
    let mut live = vec![false; len];
    let mut handler_live = vec![false; ranges.len()];
    let mut pending = vec![0];
    while let Some(p) = pending.pop() {
        if p >= len || live[p] {
            continue;
        }
        live[p] = true;
        let node = &nodes[p];
        if falls_through(&node.0) {
            pending.push(p + 1);
        }
        if let Some((to, late)) = branch_target(node, flow) {
            pending.push(if late { at.late[to] } else { at.early[to] });
        }
        for to in switch_targets(&node.0) {
            pending.push(early(to)?);
        }
        for (h, &(start, end, entry)) in ranges.iter().enumerate() {
            if (start..end).contains(&p) {
                handler_live[h] = true;
                pending.push(entry);
            }
        }
    }
    Some((live, handler_live))
}

/// Per `LineNumberTable` entry, whether kotlinc's `shouldRemove` drops it.
fn removed_lines(nodes: &[(Insn, Placement)], live: &[bool], flow: &Flow) -> Vec<bool> {
    enum Event {
        Line(usize),
        Node(usize),
    }
    let groups = flow.lines.iter().map(|&(k, _)| k + 1).max().unwrap_or(0);
    let mut lines_at: Vec<Vec<usize>> = vec![Vec::new(); groups];
    for (entry, &(k, _)) in flow.lines.iter().enumerate() {
        lines_at[k].push(entry);
    }
    // The instruction list in order: each group's line numbers, then its nodes, with a line that
    // stands after inserted instructions following the group's `Before` nodes.
    let mut events = Vec::with_capacity(nodes.len() + flow.lines.len());
    let mut next = 0;
    for (k, entries) in lines_at.iter().enumerate() {
        while next < nodes.len() && nodes[next].1.group() < k {
            events.push(Event::Node(next));
            next += 1;
        }
        if (flow.line_after_inserted)(k) {
            while next < nodes.len() && nodes[next].1 == Placement::Before(k) {
                events.push(Event::Node(next));
                next += 1;
            }
        }
        events.extend(entries.iter().map(|&entry| Event::Line(entry)));
    }
    events.extend((next..nodes.len()).map(Event::Node));
    let mut removed = vec![false; flow.lines.len()];
    for (at, event) in events.iter().enumerate() {
        let Event::Line(entry) = *event else {
            continue;
        };
        let line = flow.lines[entry].1;
        let mut passed_dead = false;
        removed[entry] = 'scan: {
            for event in &events[at + 1..] {
                match *event {
                    Event::Line(other) if flow.lines[other].1 == line => {}
                    Event::Line(_) => break 'scan passed_dead,
                    Event::Node(p) if live[p] => break 'scan false,
                    Event::Node(_) => passed_dead = true,
                }
            }
            true
        };
    }
    removed
}

/// Remove every instruction the method's entry does not reach from `nodes`, with the line numbers,
/// protected ranges and local variables that go with them; `None` when nothing is dead (or the
/// body is outside what the analysis models), leaving `nodes` as it was.
pub(crate) fn eliminate(nodes: &mut Vec<(Insn, Placement)>, flow: &Flow) -> Option<Elimination> {
    if nodes.is_empty()
        || nodes.iter().any(|(insn, _)| is_subroutine(insn))
        || nodes
            .windows(2)
            .any(|pair| pair[0].1.group() > pair[1].1.group())
    {
        return None;
    }
    let groups = flow
        .handlers
        .iter()
        .flat_map(|handler| [handler.start, handler.end, handler.handler])
        .chain(flow.lines.iter().map(|&(k, _)| k))
        .chain(
            flow.locals
                .iter()
                .flatten()
                .flat_map(|&(start, end)| [start, end]),
        )
        .chain(nodes.iter().filter_map(|node| {
            branch_target(node, flow)
                .map(|(to, _)| to)
                .into_iter()
                .chain(switch_targets(&node.0))
                .max()
        }))
        .chain(nodes.last().map(|(_, placement)| placement.group()))
        .max()?
        + 1;
    let at = Positions::of(nodes, groups);
    let (live, handler_live) = liveness(nodes, flow, &at)?;
    if live.iter().all(|&live| live) {
        return None;
    }
    let removed_lines = removed_lines(nodes, &live, flow);
    let range_holds = |range: &Option<(usize, usize)>, keep: &dyn Fn(usize) -> bool| {
        range.is_some_and(|(start, end)| {
            (at.early.get(start).copied().unwrap_or(nodes.len())
                ..at.early.get(end).copied().unwrap_or(nodes.len()))
                .any(keep)
        })
    };
    let removed_locals = flow
        .locals
        .iter()
        .map(|range| range_holds(range, &|_| true) && !range_holds(range, &|p| live[p]))
        .collect();
    let mut index = 0;
    nodes.retain(|_| {
        index += 1;
        live[index - 1]
    });
    Some(Elimination {
        removed_lines,
        removed_handlers: handler_live.iter().map(|&live| !live).collect(),
        removed_locals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IFEQ: u8 = 0x99;
    const IFNE: u8 = 0x9a;
    const IRETURN: u8 = 0xac;

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

    fn original(insns: &[Insn]) -> Vec<(Insn, Placement)> {
        insns
            .iter()
            .enumerate()
            .map(|(index, insn)| (insn.clone(), Placement::Original(index)))
            .collect()
    }

    struct Tables<'a> {
        handlers: &'a [Handler],
        lines: &'a [(usize, u16)],
        locals: &'a [Option<(usize, usize)>],
        late: &'a [usize],
    }

    const NONE: Tables<'static> = Tables {
        handlers: &[],
        lines: &[],
        locals: &[],
        late: &[],
    };

    fn run(
        nodes: &mut Vec<(Insn, Placement)>,
        tables: &Tables,
    ) -> Option<(Vec<Insn>, Elimination)> {
        let flow = Flow {
            handlers: tables.handlers,
            late_branch: &|index| tables.late.contains(&index),
            lines: tables.lines,
            line_after_inserted: &|_| false,
            locals: tables.locals,
        };
        let elimination = eliminate(nodes, &flow)?;
        Some((
            nodes.iter().map(|(insn, _)| insn.clone()).collect(),
            elimination,
        ))
    }

    /// `if (i == k) return 1` as a loop body's last statement, after its jump was threaded past the
    /// `goto` back to the loop head: 0 iload_0; 1 ifeq 5; 2 iconst_1; 3 ireturn; 4 goto 5;
    /// 5 iconst_0; 6 ireturn.
    fn threaded() -> Vec<(Insn, Placement)> {
        original(&[
            op(0x1a),
            branch(IFEQ, 5),
            op(0x04),
            op(IRETURN),
            branch(GOTO, 5),
            op(0x03),
            op(IRETURN),
        ])
    }

    #[test]
    fn a_goto_nothing_reaches_is_removed_with_its_line() {
        let mut nodes = threaded();
        let (insns, elimination) = run(
            &mut nodes,
            &Tables {
                lines: &[(0, 1), (4, 2), (5, 3)],
                ..NONE
            },
        )
        .expect("the goto is dead");
        assert_eq!(
            insns,
            vec![
                op(0x1a),
                branch(IFEQ, 5),
                op(0x04),
                op(IRETURN),
                op(0x03),
                op(IRETURN),
            ]
        );
        assert_eq!(elimination.removed_lines, vec![false, true, false]);
    }

    #[test]
    fn a_line_whose_scan_reaches_a_live_instruction_of_the_same_line_stays() {
        // The scan passes the dead `goto`, skips the next entry of the same line, and stops at the
        // live `iconst_0`.
        let mut nodes = threaded();
        let (_, elimination) = run(
            &mut nodes,
            &Tables {
                lines: &[(4, 2), (5, 2)],
                ..NONE
            },
        )
        .expect("the goto is dead");
        assert_eq!(elimination.removed_lines, vec![false, false]);
    }

    #[test]
    fn a_line_followed_directly_by_another_line_stays() {
        // 0 iconst_0; 1 ireturn; 2 goto 0 — two lines stand at the dead `goto`: the first reaches the
        // second with no instruction between them, the second only dead code before the end.
        let mut nodes = original(&[op(0x03), op(IRETURN), branch(GOTO, 0)]);
        let (insns, elimination) = run(
            &mut nodes,
            &Tables {
                lines: &[(0, 4), (2, 5), (2, 6)],
                ..NONE
            },
        )
        .expect("the goto is dead");
        assert_eq!(insns, vec![op(0x03), op(IRETURN)]);
        assert_eq!(elimination.removed_lines, vec![false, false, true]);
    }

    #[test]
    fn a_protected_range_left_with_only_dead_code_goes_with_its_handler() {
        // 0 iconst_0; 1 ireturn; 2 nop (protected, dead); 3 astore_0 (handler); 4 iconst_1;
        // 5 ireturn.
        let mut nodes = original(&[
            op(0x03),
            op(IRETURN),
            op(0x00),
            op(0x4b),
            op(0x04),
            op(IRETURN),
        ]);
        let handlers = [Handler {
            start: 2,
            end: 3,
            handler: 3,
        }];
        let (insns, elimination) = run(
            &mut nodes,
            &Tables {
                handlers: &handlers,
                locals: &[Some((3, 6)), Some((0, 6))],
                ..NONE
            },
        )
        .expect("the range and its handler are dead");
        assert_eq!(insns, vec![op(0x03), op(IRETURN)]);
        assert_eq!(elimination.removed_handlers, vec![true]);
        assert_eq!(elimination.removed_locals, vec![true, false]);
    }

    #[test]
    fn a_live_protected_range_keeps_its_handler_live() {
        // 0 iconst_0; 1 ireturn (protected); 2 astore_0 (handler); 3 iconst_1; 4 ireturn;
        // 5 goto 0 (dead).
        let mut nodes = original(&[
            op(0x03),
            op(IRETURN),
            op(0x4b),
            op(0x04),
            op(IRETURN),
            branch(GOTO, 0),
        ]);
        let handlers = [Handler {
            start: 0,
            end: 2,
            handler: 2,
        }];
        let (insns, elimination) = run(
            &mut nodes,
            &Tables {
                handlers: &handlers,
                ..NONE
            },
        )
        .expect("the trailing goto is dead");
        assert_eq!(
            insns,
            vec![op(0x03), op(IRETURN), op(0x4b), op(0x04), op(IRETURN)]
        );
        assert_eq!(elimination.removed_handlers, vec![false]);
    }

    #[test]
    fn a_late_jump_skips_the_instruction_inserted_before_its_target() {
        // 0 iload_0; 1 ifne 3 (to the label after the inserted `pop`); 2 ireturn; pop (inserted
        // before 3); 3 iconst_0; 4 ireturn.
        let nodes = vec![
            (op(0x1a), Placement::Original(0)),
            (branch(IFNE, 3), Placement::Original(1)),
            (op(IRETURN), Placement::Original(2)),
            (op(0x57), Placement::Before(3)),
            (op(0x03), Placement::Original(3)),
            (op(IRETURN), Placement::Original(4)),
        ];
        let (insns, _) =
            run(&mut nodes.clone(), &Tables { late: &[1], ..NONE }).expect("the pop is dead");
        assert_eq!(
            insns,
            vec![
                op(0x1a),
                branch(IFNE, 3),
                op(IRETURN),
                op(0x03),
                op(IRETURN)
            ]
        );
        assert!(run(&mut nodes.clone(), &NONE).is_none());
    }

    #[test]
    fn a_method_without_dead_code_is_left_alone() {
        let mut nodes = original(&[op(0x1a), branch(IFEQ, 3), op(0x04), op(0x03), op(IRETURN)]);
        assert!(run(&mut nodes, &NONE).is_none());
    }
}
