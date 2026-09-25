//! kotlinc's `RedundantNopsCleanupMethodTransformer`, over a [`MethodNode`]: every `nop` in the body
//! goes, except the first `nop` of a debug range (from a line number or a local variable's range
//! boundary up to the next line number) that holds no other instruction, and a `nop` that is the
//! first instruction of a protected range.
//!
//! The `nop`s come from `redundant_gotos`, which leaves one where it removes a `goto`, and from
//! inlined bodies, which bring their own, including kotlinc's leading one and those before
//! rewritten returns.

use std::collections::BTreeSet;

use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

const NOP: u8 = 0x00;

fn is_nop(node: &Node) -> bool {
    matches!(node, Node::Insn(Insn::Op(NOP)))
}

/// Remove every `nop` [`required_nops`] does not keep; `true` when any went.
pub(crate) fn remove(method: &mut MethodNode) -> bool {
    if !method.nodes.iter().any(is_nop) {
        return false;
    }
    let required = required_nops(method);
    let removed: Vec<usize> = method
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(at, node)| (is_nop(node) && !required.contains(&at)).then_some(at))
        .collect();
    for &at in removed.iter().rev() {
        method.nodes.remove(at);
    }
    !removed.is_empty()
}

/// The `nop`s kotlinc's cleanup keeps, by node position.
pub(crate) fn required_nops(method: &MethodNode) -> BTreeSet<usize> {
    let nodes = &method.nodes;
    let position = |label: LabelId| {
        nodes
            .iter()
            .position(|node| *node == Node::Label(label))
            .expect("a label the method names stands in it")
    };
    let mut required = BTreeSet::new();
    for block in &method.try_catch_blocks {
        let first =
            (position(block.start)..nodes.len()).find(|&at| matches!(nodes[at], Node::Insn(_)));
        if let Some(at) = first.filter(|&at| is_nop(&nodes[at])) {
            required.insert(at);
        }
    }
    // Debug points in list order: a local variable's range boundary, a line number.
    let bounds: BTreeSet<LabelId> = method
        .local_variables
        .iter()
        .flat_map(|local| [local.start, local.end])
        .collect();
    let points: Vec<(usize, bool)> = nodes
        .iter()
        .enumerate()
        .filter_map(|(at, node)| match node {
            Node::Label(label) if bounds.contains(label) => Some((at, false)),
            Node::Line { .. } => Some((at, true)),
            _ => None,
        })
        .collect();
    for pair in points.windows(2) {
        // kotlinc's `recordNopsRequiredForDebugger`: a stretch that starts at a line number and
        // runs to the next debug point keeps a `nop` when it has no other instruction.
        let [(from, true), (to, _)] = *pair else {
            continue;
        };
        let mut first_nop = None;
        let mut only_nops = true;
        for (at, node) in nodes.iter().enumerate().take(to).skip(from) {
            match node {
                _ if is_nop(node) => {
                    first_nop.get_or_insert(at);
                }
                Node::Insn(_) => {
                    only_nops = false;
                    break;
                }
                _ => {}
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
    use crate::jvm::bytecode_passes::labelled_body::{labelled_body, ops, LabelledBody};
    use crate::jvm::method_node::TryCatchBlock;

    const GOTO: u8 = 0xa7;
    const IRETURN: u8 = 0xac;

    /// The body's instructions after the cleanup, or `None` when it removed nothing.
    fn run(mut body: LabelledBody) -> Option<Vec<Insn>> {
        remove(&mut body.method).then(|| body.method.instructions().cloned().collect())
    }

    #[test]
    fn a_nop_the_body_already_had_is_cleaned_up() {
        // An inlined body's leading `nop` and the one in front of its rewritten return go, as
        // kotlinc's cleanup removes every `nop` it does not need.
        let body = labelled_body(&[Ok(NOP), Ok(0x03), Ok(NOP), Ok(IRETURN)], &[0], None);
        assert_eq!(run(body), Some(ops(&[0x03, IRETURN])));
    }

    #[test]
    fn a_nop_sharing_its_line_goes() {
        // 0 iconst_0; 1 goto 3; 2 (a line starts here) nop; 3 ireturn: the `nop` shares its line
        // with the `ireturn`.
        let body = labelled_body(
            &[Ok(0x03), Err((GOTO, 3)), Ok(NOP), Ok(IRETURN)],
            &[0, 2],
            None,
        );
        let mut expected = ops(&[0x03]);
        expected.push(body.jump(GOTO, 3));
        expected.push(Insn::Op(IRETURN));
        assert_eq!(run(body), Some(expected));
    }

    #[test]
    fn only_a_stretch_that_starts_at_a_line_keeps_its_nop() {
        // 0 (line) iconst_0; 1 istore_0; 2 nop; 3 (line) iload_0; 4 ireturn, where `x` ends at 2: the
        // `nop` lies between the end of `x` and a line, a stretch no line starts, so it goes. An
        // inlined lambda's `nop`s after its locals end are removed this way.
        let after_end = labelled_body(
            &[Ok(0x03), Ok(0x3b), Ok(NOP), Ok(0x1a), Ok(IRETURN)],
            &[0, 3],
            Some((0, 2)),
        );
        assert_eq!(run(after_end), Some(ops(&[0x03, 0x3b, 0x1a, IRETURN])));
        // A line whose only instruction is the `nop`, up to the end of `x`, keeps it: nothing
        // changes.
        let alone_on_its_line = labelled_body(
            &[Ok(0x03), Ok(0x3b), Ok(NOP), Ok(0x1a), Ok(IRETURN)],
            &[0, 2],
            Some((0, 3)),
        );
        assert_eq!(run(alone_on_its_line), None);
    }

    #[test]
    fn a_nop_alone_on_its_line_stays() {
        // 0 iconst_0; 1 istore_0; 2 (line) nop; 3 (line) iload_0; 4 ireturn: the `nop` a removed
        // `goto` left is the only instruction of its line, so kotlinc keeps it.
        let body = labelled_body(
            &[Ok(0x03), Ok(0x3b), Ok(NOP), Ok(0x1a), Ok(IRETURN)],
            &[0, 2, 3],
            None,
        );
        assert_eq!(run(body), None);
    }

    #[test]
    fn a_nop_that_starts_a_protected_range_stays() {
        // 0 nop; 1 iconst_0; 2 nop; 3 ireturn, with a range protecting only the second `nop`.
        let mut body = labelled_body(&[Ok(NOP), Ok(0x03), Ok(NOP), Ok(IRETURN)], &[], None);
        let labels = body.labels.clone();
        body.method.try_catch_blocks.push(TryCatchBlock {
            start: labels[2],
            end: labels[3],
            handler: labels[3],
            catch_type: None,
        });
        assert_eq!(run(body), Some(ops(&[0x03, NOP, IRETURN])));
    }
}
