//! The control-flow graph of a decoded method body, in instruction indices.
//!
//! [`Insn`] already carries branch targets as instruction indices rather than byte offsets, so a
//! graph built here survives any transform that inserts or removes instructions — which is the whole
//! reason the coroutine transform can run on bytecode at all.

use crate::jvm::inline::{BranchTarget, Insn};

/// One exception-table entry in INSTRUCTION indices: `[start, end)` is protected, `handler` is the
/// entry point. The catch type is irrelevant to control flow — any of them may be taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Handler {
    pub start: usize,
    pub end: usize,
    pub handler: usize,
}

/// Successors of every instruction, with normal and exceptional edges kept apart.
///
/// They are separate because a kill does not apply along an exceptional edge: the throw may happen
/// before the store that would have killed the local completes, so a backward analysis must let the
/// handler's demand through unfiltered.
pub(crate) struct ControlGraph {
    normal: Vec<Vec<usize>>,
    exceptional: Vec<Vec<usize>>,
    /// One past the last instruction — the virtual exit. A `goto` to it is how a spliced body's
    /// redirected returns leave the region (see `inline::redirect_returns`), so it is a real target.
    exit: usize,
}

/// Whether `op` leaves the method (or the region) without a fall-through successor.
fn is_terminator(op: u8) -> bool {
    matches!(op, 0xac..=0xb1 | 0xbf) // ?return, return, athrow
}

impl ControlGraph {
    /// `None` when the body uses the deprecated subroutine instructions (`jsr`/`jsr_w`/`ret`),
    /// whose successor relation is not expressible here. No Kotlin compiler emits them; refusing is
    /// what keeps that assumption honest rather than silently analyzing a wrong graph.
    pub(crate) fn build(insns: &[Insn], handlers: &[Handler]) -> Option<ControlGraph> {
        let exit = insns.len();
        let mut normal = vec![Vec::new(); exit + 1];
        let mut exceptional = vec![Vec::new(); exit + 1];
        for (index, insn) in insns.iter().enumerate() {
            match insn {
                Insn::Plain { op: 0xa9, .. } => return None, // ret
                Insn::Plain { op, .. } => {
                    if !is_terminator(*op) {
                        normal[index].push(index + 1);
                    }
                }
                Insn::Branch { op: 0xa8, .. } | Insn::BranchW { op: 0xc9, .. } => return None, // jsr
                Insn::Branch { op, target } | Insn::BranchW { op, target } => {
                    // An EXTERNAL target belongs to an enclosing builder and is not part of this
                    // graph; the transfer leaves the region and has no successor inside it.
                    if let BranchTarget::Internal(to) = target {
                        if *to > exit {
                            return None;
                        }
                        normal[index].push(*to);
                    }
                    let unconditional = matches!(op, 0xa7 | 0xc8);
                    if !unconditional {
                        normal[index].push(index + 1);
                    }
                }
                Insn::TableSwitch {
                    default, targets, ..
                } => {
                    for &to in std::iter::once(default).chain(targets) {
                        if to > exit {
                            return None;
                        }
                        normal[index].push(to);
                    }
                }
                Insn::LookupSwitch { default, pairs } => {
                    for &to in std::iter::once(default).chain(pairs.iter().map(|(_, to)| to)) {
                        if to > exit {
                            return None;
                        }
                        normal[index].push(to);
                    }
                }
            }
        }
        for entry in handlers {
            if entry.handler > exit || entry.start > entry.end || entry.end > exit {
                return None;
            }
            for protected in exceptional.iter_mut().take(entry.end).skip(entry.start) {
                if !protected.contains(&entry.handler) {
                    protected.push(entry.handler);
                }
            }
        }
        Some(ControlGraph {
            normal,
            exceptional,
            exit,
        })
    }

    pub(crate) fn exit(&self) -> usize {
        self.exit
    }

    pub(crate) fn normal_successors(&self, index: usize) -> &[usize] {
        self.normal.get(index).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn exceptional_successors(&self, index: usize) -> &[usize] {
        self.exceptional
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Instruction indices reachable from entry, in reverse post-order — the visit order that makes
    /// a backward fixpoint converge in few sweeps.
    pub(crate) fn reverse_post_order(&self) -> Vec<usize> {
        let mut order = Vec::new();
        let mut state = vec![0u8; self.exit + 1]; // 0 unseen, 1 on stack, 2 done
        let mut stack = vec![(0usize, 0usize)];
        if self.exit == 0 {
            return order;
        }
        state[0] = 1;
        while let Some((index, next)) = stack.pop() {
            let successors = self
                .normal_successors(index)
                .iter()
                .chain(self.exceptional_successors(index));
            match successors.skip(next).next() {
                Some(&to) => {
                    stack.push((index, next + 1));
                    if state[to] == 0 {
                        state[to] = 1;
                        stack.push((to, 0));
                    }
                }
                None => {
                    state[index] = 2;
                    order.push(index);
                }
            }
        }
        order.reverse();
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(op: u8) -> Insn {
        Insn::Plain {
            op,
            operands: Vec::new(),
        }
    }

    fn goto(target: usize) -> Insn {
        Insn::Branch {
            op: 0xa7,
            target: BranchTarget::Internal(target),
        }
    }

    fn if_eq(target: usize) -> Insn {
        Insn::Branch {
            op: 0x99,
            target: BranchTarget::Internal(target),
        }
    }

    #[test]
    fn straight_line_code_falls_through() {
        let insns = [plain(0x00), plain(0x00), plain(0xb1)]; // nop, nop, return
        let graph = ControlGraph::build(&insns, &[]).expect("graph");
        assert_eq!(graph.normal_successors(0), [1]);
        assert_eq!(graph.normal_successors(1), [2]);
        assert_eq!(graph.normal_successors(2), [] as [usize; 0]);
    }

    #[test]
    fn a_conditional_branches_and_falls_through_but_a_goto_only_branches() {
        let insns = [if_eq(3), goto(3), plain(0x00), plain(0xb1)];
        let graph = ControlGraph::build(&insns, &[]).expect("graph");
        assert_eq!(graph.normal_successors(0), [3, 1]);
        assert_eq!(graph.normal_successors(1), [3]);
    }

    #[test]
    fn athrow_has_no_normal_successor() {
        let insns = [plain(0xbf), plain(0xb1)];
        let graph = ControlGraph::build(&insns, &[]).expect("graph");
        assert_eq!(graph.normal_successors(0), [] as [usize; 0]);
    }

    #[test]
    fn a_switch_reaches_every_arm_and_its_default() {
        let insns = [
            Insn::TableSwitch {
                default: 3,
                low: 0,
                targets: vec![1, 2],
            },
            plain(0xb1),
            plain(0xb1),
            plain(0xb1),
        ];
        let graph = ControlGraph::build(&insns, &[]).expect("graph");
        assert_eq!(graph.normal_successors(0), [3, 1, 2]);
    }

    #[test]
    fn a_protected_range_reaches_its_handler_from_every_instruction_in_it() {
        let insns = [plain(0x00), plain(0x00), plain(0x00), plain(0xb1)];
        let handlers = [Handler {
            start: 1,
            end: 3,
            handler: 3,
        }];
        let graph = ControlGraph::build(&insns, &handlers).expect("graph");
        assert_eq!(graph.exceptional_successors(0), [] as [usize; 0]);
        assert_eq!(graph.exceptional_successors(1), [3]);
        assert_eq!(graph.exceptional_successors(2), [3]);
        assert_eq!(graph.exceptional_successors(3), [] as [usize; 0]);
    }

    /// A spliced body's returns are redirected to `insns.len()` — one past the end — so that index
    /// is a legitimate branch target and must not be rejected as out of range.
    #[test]
    fn one_past_the_end_is_a_reachable_exit() {
        let insns = [goto(1)];
        let graph = ControlGraph::build(&insns, &[]).expect("graph");
        assert_eq!(graph.normal_successors(0), [1]);
        assert_eq!(graph.exit(), 1);
        assert_eq!(graph.normal_successors(1), [] as [usize; 0]);
    }

    #[test]
    fn the_deprecated_subroutine_instructions_are_refused() {
        for insn in [
            plain(0xa9), // ret
            Insn::Branch {
                op: 0xa8,
                target: BranchTarget::Internal(0),
            },
            Insn::BranchW {
                op: 0xc9,
                target: BranchTarget::Internal(0),
            },
        ] {
            assert!(ControlGraph::build(&[insn, plain(0xb1)], &[]).is_none());
        }
    }

    #[test]
    fn reverse_post_order_visits_a_block_before_its_successors() {
        let insns = [if_eq(3), plain(0x00), goto(4), plain(0x00), plain(0xb1)];
        let graph = ControlGraph::build(&insns, &[]).expect("graph");
        let order = graph.reverse_post_order();
        let position = |index: usize| order.iter().position(|&i| i == index).expect("reached");
        assert!(position(0) < position(1));
        assert!(position(0) < position(3));
        assert!(position(1) < position(2));
        assert!(position(2) < position(4));
    }
}
