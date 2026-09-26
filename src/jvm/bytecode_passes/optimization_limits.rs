//! kotlinc's applicability gates for its optimization passes (`OptimizationMethodVisitor`'s
//! companion): a pass declines a method whose analysis frames would weigh too much, and leaves it
//! as it is.
//!
//! The frame count is `countInsnsWithFramesUntil`: over ASM's node list, every instruction with an
//! opcode other than `nop`, and every other node (a label, a line number, a `nop`) that a label
//! follows. A label is in ASM's list only when something refers to it. A frame weighs
//! `max_locals + max_stack` values, and a method's weight is in MiB (`getTotalFramesWeight`).
//!
//! kotlinc multiplies the counts as a 32-bit `Int`, so a product past `Int.MAX_VALUE` wraps. Here
//! every product is checked instead, and a method whose weight does not fit is declined: the gate
//! is a resource bound first.

use std::collections::{BTreeSet, HashMap};

use crate::jvm::method_node::{LabelId, MethodNode, Node};

/// `MEMORY_LIMIT_BY_METHOD_MB`.
const MEMORY_LIMIT_MB: u64 = 50;

/// `TRY_CATCH_BLOCKS_SOFT_LIMIT`: past this many handlers, the handlers' ranges count too.
const TRY_CATCH_BLOCKS_SOFT_LIMIT: usize = 16;

const NOP: u8 = 0x00;

const MIB: u64 = 1024 * 1024;

/// `canBeOptimizedUsingSourceInterpreter`: whether a pass analyzing `method` with a source
/// interpreter may run. Such an analysis holds, per frame and value, a set of the instructions
/// that can have pushed it, so its weight counts the frames squared.
pub(crate) fn fits_source_interpreter(method: &MethodNode) -> bool {
    let counts = FrameCounts::of(method);
    source_interpreter_fits(
        counts.frames,
        method.try_catch_blocks.len(),
        counts.handler_ranges(method),
        usize::from(method.max_locals),
        usize::from(method.max_stack),
    )
}

/// The gate over its counts: `frames` in the method, `handlers` try/catch blocks whose ranges hold
/// `handler_frames` frames together (`None` when that sum does not fit), and a frame's width.
fn source_interpreter_fits(
    frames: usize,
    handlers: usize,
    handler_frames: Option<usize>,
    max_locals: usize,
    max_stack: usize,
) -> bool {
    let weight = |size: Option<usize>| -> Option<u64> {
        let width = u64::try_from(max_locals.checked_add(max_stack)?).ok()?;
        u64::try_from(size?)
            .ok()?
            .checked_mul(width)
            .map(|cells| cells / MIB)
    };
    if handlers > TRY_CATCH_BLOCKS_SOFT_LIMIT {
        let handler_weight = weight(handler_frames.and_then(|size| size.checked_mul(frames)));
        if handler_weight.is_none_or(|weight| weight > MEMORY_LIMIT_MB) {
            return false;
        }
    }
    weight(frames.checked_mul(frames)).is_some_and(|weight| weight < MEMORY_LIMIT_MB)
}

/// Where the frames of a method are, over ASM's node list.
struct FrameCounts {
    /// Per node of the method, how many frames the nodes before it hold.
    before: Vec<usize>,
    /// The frames of the whole method.
    frames: usize,
}

impl FrameCounts {
    fn of(method: &MethodNode) -> FrameCounts {
        let referenced = method.referenced_labels();
        let listed =
            |node: &Node| !matches!(node, Node::Label(label) if !referenced.contains(label));
        // Per node, whether the next node in ASM's list is a label.
        let mut label_next = vec![false; method.nodes.len()];
        let mut next_is_label = false;
        for (at, node) in method.nodes.iter().enumerate().rev() {
            label_next[at] = next_is_label;
            if listed(node) {
                next_is_label = matches!(node, Node::Label(_));
            }
        }
        let mut before = Vec::with_capacity(method.nodes.len() + 1);
        let mut frames = 0usize;
        for (node, label_next) in method.nodes.iter().zip(label_next) {
            before.push(frames);
            let counted = match node {
                Node::Insn(insn) => super::analysis::opcode(insn) != NOP || label_next,
                Node::Label(_) | Node::Line { .. } => listed(node) && label_next,
            };
            frames += usize::from(counted);
        }
        before.push(frames);
        FrameCounts { before, frames }
    }

    /// `getTotalTcbSize`: the frames from each handler range's start label up to its end label,
    /// summed; `None` when the sum does not fit.
    fn handler_ranges(&self, method: &MethodNode) -> Option<usize> {
        let wanted: BTreeSet<LabelId> = method
            .try_catch_blocks
            .iter()
            .flat_map(|block| [block.start, block.end])
            .collect();
        let at: HashMap<LabelId, usize> = method
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| match node {
                Node::Label(label) if wanted.contains(label) => Some((*label, index)),
                _ => None,
            })
            .collect();
        let end = self.before.len() - 1;
        method
            .try_catch_blocks
            .iter()
            .try_fold(0usize, |sum, block| {
                let start = at.get(&block.start).copied().unwrap_or(end);
                let stop = at
                    .get(&block.end)
                    .copied()
                    .filter(|&stop| stop >= start)
                    .unwrap_or(end);
                sum.checked_add(self.before[stop] - self.before[start])
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::method_node::{Insn, TryCatchBlock};

    const ICONST_0: u8 = 0x03;
    const GOTO: u8 = 0xa7;
    const RETURN: u8 = 0xb1;

    fn op(code: u8) -> Node {
        Node::Insn(Insn::Op(code))
    }

    #[test]
    fn a_frame_is_an_instruction_or_a_node_a_label_follows() {
        let mut method = MethodNode::new(0x0009, "f", "()V");
        let named = method.new_label();
        let loose = method.new_label();
        let line = method.new_label();
        method.nodes = vec![
            op(ICONST_0), // counted: an instruction
            op(NOP),      // not counted: a `nop` with an instruction next
            op(NOP),      // counted: the named label follows
            Node::Label(named),
            Node::Label(loose), // not in ASM's list
            Node::Label(line),  // counted: named by the line, and a label follows
            Node::Label(named),
            Node::Line {
                line: 1,
                start: line,
            }, // not counted: an instruction follows
            op(RETURN), // counted
            Node::Insn(Insn::Jump {
                op: GOTO,
                target: named,
            }), // counted
        ];
        // `named` at 3 is followed by `loose`, which ASM does not hold, then by `line`: counted.
        // `named` at 6 is followed by the line number: not counted.
        assert_eq!(FrameCounts::of(&method).frames, 6);
    }

    #[test]
    fn a_handler_range_counts_its_frames() {
        let mut method = MethodNode::new(0x0009, "f", "()V");
        let start = method.new_label();
        let end = method.new_label();
        let handler = method.new_label();
        method.nodes = vec![
            Node::Label(start),
            op(ICONST_0),
            op(ICONST_0),
            Node::Label(end),
            op(RETURN),
            Node::Label(handler),
            op(RETURN),
        ];
        method.try_catch_blocks = vec![TryCatchBlock {
            start,
            end,
            handler,
            catch_type: None,
        }];
        let counts = FrameCounts::of(&method);
        // The second `iconst_0` is followed by `end`, but is an instruction anyway.
        assert_eq!(counts.handler_ranges(&method), Some(2));
    }

    #[test]
    fn the_weight_is_the_frames_squared_times_the_width_in_mib() {
        // 1,001 frames: 1,002,001 cells per unit of width; 52 wide is 49.7 MiB, 53 wide 50.6 MiB.
        assert!(source_interpreter_fits(1_001, 0, Some(0), 51, 1));
        assert!(!source_interpreter_fits(1_001, 0, Some(0), 52, 1));
        // The width counts the operand stack as much as the locals.
        assert!(!source_interpreter_fits(1_001, 0, Some(0), 1, 52));
    }

    #[test]
    fn many_handlers_count_their_ranges() {
        // Past 16 handlers, handler frames times the method's frames must weigh at most 50 MiB.
        assert!(source_interpreter_fits(100, 16, None, 1, 1));
        assert!(!source_interpreter_fits(100, 17, None, 1, 1));
        // 26,738 * 1,000 * 2 is 50.99 MiB, 26,739 * 1,000 * 2 is 51.00.
        assert!(source_interpreter_fits(1_000, 17, Some(26_738), 1, 1));
        assert!(!source_interpreter_fits(1_000, 17, Some(26_739), 1, 1));
    }

    #[test]
    fn a_weight_that_does_not_fit_declines() {
        assert!(!source_interpreter_fits(usize::MAX, 0, Some(0), 1, 1));
        assert!(!source_interpreter_fits(3, 0, Some(0), usize::MAX, 1));
        assert!(!source_interpreter_fits(3, 0, Some(0), 1, usize::MAX));
        assert!(!source_interpreter_fits(2, 17, Some(usize::MAX), 1, 1));
    }
}
