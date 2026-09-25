//! The try/catch part of kotlinc's `FixStackMethodTransformer`, which `preprocessNodeBeforeInline`
//! runs on every body before it is analyzed (`insertTryCatchBlocksMarkers`). Each protected range
//! gets a fresh start label right before the instruction that opens it (kotlinc's `nop`), after the line number its old
//! start carries. kotlinc also saves a non-empty operand stack there; a body read from a class file
//! was already fixed when it was compiled, so its stack is empty at every try and the markers that
//! would save it are removed again unchanged.
//!
//! The move matters to `markPlacesForInlineAndRemoveInlinable`: its analyzer follows an exception
//! edge only from a store or from the node right after a range's start, which must be the `nop`
//! rather than a line number for a handler to be reached at all.

use std::collections::{HashMap, HashSet};

use crate::jvm::method_node::{LabelId, MethodNode, Node};

use super::preparation::label_positions;
use super::InlineError;

/// Give every try's range a start label right before its first instruction, and move the ranges that
/// start at the old label onto it (`collectDecompiledTryDescriptors`, `transformTryCatchBlocks`).
pub(super) fn move_try_starts_to_their_first_instruction(
    node: &mut MethodNode,
) -> Result<(), InlineError> {
    // One try per handler; a range whose handler an earlier range already has belongs to that try
    // (a `finally` split around a jump), and its start does not open a try.
    let mut handlers = HashSet::new();
    let mut try_starts: Vec<LabelId> = Vec::new();
    for block in &node.try_catch_blocks {
        if !handlers.insert(block.handler) {
            continue;
        }
        if !try_starts.contains(&block.start) {
            try_starts.push(block.start);
        }
    }
    let mut moved: HashMap<LabelId, LabelId> = HashMap::new();
    for start in try_starts {
        let positions = label_positions(node);
        let from = positions[start.index()].ok_or_else(|| {
            InlineError::Analysis("a try block starts at an unplaced label".into())
        })?;
        // kotlinc asserts the instruction is the `nop` only with assertions enabled; it moves the
        // start before whatever instruction comes first.
        let Some(at) = node.nodes[from..]
            .iter()
            .position(|entry| matches!(entry, Node::Insn(_)))
            .map(|offset| from + offset)
        else {
            continue;
        };
        let label = node.new_label();
        node.nodes.insert(at, Node::Label(label));
        moved.insert(start, label);
    }
    for block in &mut node.try_catch_blocks {
        if let Some(&label) = moved.get(&block.start) {
            block.start = label;
        }
    }
    Ok(())
}
