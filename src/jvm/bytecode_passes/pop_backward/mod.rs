//! kotlinc's `PopBackwardPropagationTransformer` over a [`MethodNode`]: a `pop` of a value that
//! nothing else needs goes, and so does the pushing of that value where it is pure.
//!
//! The analysis ([`sources`]) finds, for every value, the instructions that can have pushed it,
//! and marks as untouchable (`dontTouch`) every pusher whose value some instruction other than a
//! `pop`/`pop2`/`return` consumes. Then, in instruction order:
//!
//! - a `pop` whose pushers include an untouchable one, or which would get longer fused with them
//!   (`longerWhenFusedWithPop`: each pure push counts -1, a primitive boxing or conversion 0,
//!   anything else +1, and a positive sum is longer), marks all its pushers untouchable;
//! - a `pop2`, `dup_x1`, `dup_x2`, `dup2_x1` or `dup2_x2` marks the pushers of the values it
//!   reaches (by words: 2; 1 and 1; 1 and 2; 2 and 1; 2 and 2) untouchable.
//!
//! Then every `pop` none of whose pushers is untouchable becomes a `nop`, and each of its pushers
//! not already transformed does the pop itself: a pure push (a load, `aconst_null`…`ldc`, or
//! `getstatic kotlin/Unit.INSTANCE`) becomes a `nop`; a primitive `valueOf` or a primitive
//! conversion becomes the `pop`/`pop2` of its own input; anything else gets a `pop`/`pop2` after
//! it. A `pop2` never goes. The `nop`s are left to the later dead-code, `goto` and `nop` steps.

mod sources;

#[cfg(test)]
mod tests;

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

use super::analysis::{analyze_with, opcode, AnalyzerError, AnalyzerOptions, Frame, PlainFrames};
use super::opcodes::*;
use super::redundant_boxing::is_primitive_boxing_insn;
use crate::jvm::method_node::{Insn, MethodNode, Node};
use sources::{HazardsTracking, SourceValue};

/// What a pushing instruction, or the `pop` itself, becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transformation {
    ReplaceWithNop,
    ReplaceWithPop1,
    ReplaceWithPop2,
    InsertPop1After,
    InsertPop2After,
}

/// `isPurePush`: a load, a constant (`aconst_null`…`ldc`), or `kotlin.Unit`'s instance.
fn is_pure_push(insn: &Insn) -> bool {
    (ILOAD..=ALOAD).contains(&opcode(insn))
        || (ACONST_NULL..=LDC2_W).contains(&opcode(insn))
        || is_unit_instance(insn)
}

/// `isUnitInstance`: `getstatic kotlin/Unit.INSTANCE`.
fn is_unit_instance(insn: &Insn) -> bool {
    matches!(insn, Insn::Field { op: GETSTATIC, owner, name, .. }
        if owner == "kotlin/Unit" && name == "INSTANCE")
}

fn is_pop(insn: &Insn) -> bool {
    matches!(opcode(insn), POP | POP2)
}

/// `isPrimitiveTypeConversion`: `i2l`…`i2s`.
fn is_primitive_type_conversion(insn: &Insn) -> bool {
    (I2L..=I2S).contains(&opcode(insn))
}

fn insn_at(nodes: &[Node], index: usize) -> Option<&Insn> {
    match &nodes[index] {
        Node::Insn(insn) => Some(insn),
        _ => None,
    }
}

/// `longerWhenFusedWithPop`: whether removing the `pop` of `value` costs more instructions than it
/// saves.
fn longer_when_fused_with_pop(value: &SourceValue, nodes: &[Node]) -> bool {
    let mut balance = 0i64;
    for insn in value
        .insns
        .iter()
        .filter_map(|&index| insn_at(nodes, index))
    {
        if is_pure_push(insn) {
            balance -= 1;
        } else if !(is_primitive_boxing_insn(insn) || is_primitive_type_conversion(insn)) {
            balance += 1;
        }
    }
    balance > 0
}

/// The values making up the `first` words on top of `frame`'s stack and the `second` words below
/// them; `None` when a value straddles either boundary or the stack is too shallow
/// (`peekWords`).
fn peek_words(
    frame: &Frame<SourceValue>,
    first: usize,
    second: usize,
) -> Option<Vec<&SourceValue>> {
    let mut values = Vec::new();
    let mut below = frame.stack.iter().rev();
    for size in [first, second] {
        let mut words = 0;
        while words < size {
            let value = below.next()?;
            words += value.size;
            values.push(value);
        }
        if words > size {
            return None;
        }
    }
    Some(values)
}

/// The transformations one run decides, by node index.
struct Run<'a> {
    nodes: &'a [Node],
    frames: &'a [Option<Frame<SourceValue>>],
    dont_touch: Vec<bool>,
}

impl Run<'_> {
    fn should_keep(&self, index: usize) -> bool {
        self.dont_touch[index]
    }

    fn mark(&mut self, value: &SourceValue) {
        for &index in value.insns.iter() {
            self.dont_touch[index] = true;
        }
    }

    /// The first walk: the `pop`s and stack shuffles that keep their operands' pushers.
    fn mark_hazards(&mut self) {
        for (index, node) in self.nodes.iter().enumerate() {
            let (Node::Insn(insn), Some(frame)) = (node, &self.frames[index]) else {
                continue;
            };
            let words = match opcode(insn) {
                POP => {
                    let Some(top) = frame.stack.last() else {
                        continue;
                    };
                    if top.insns.iter().any(|&source| self.should_keep(source))
                        || longer_when_fused_with_pop(top, self.nodes)
                    {
                        self.mark(top);
                    }
                    continue;
                }
                POP2 => (2, 0),
                DUP_X1 => (1, 1),
                DUP_X2 => (1, 2),
                DUP2_X1 => (2, 1),
                DUP2_X2 => (2, 2),
                _ => continue,
            };
            let Some(values) = peek_words(frame, words.0, words.1) else {
                continue;
            };
            for value in values {
                self.mark(value);
            }
        }
    }

    /// `combineWithPop`: what `source`, a pusher of a popped value of `size` words, becomes.
    fn combine_with_pop(
        &self,
        source: usize,
        size: usize,
    ) -> Result<Transformation, AnalyzerError> {
        let error = |message: &str| AnalyzerError {
            index: source,
            message: message.to_string(),
        };
        let insn =
            insn_at(self.nodes, source).ok_or_else(|| error("a pusher that is no instruction"))?;
        if is_pure_push(insn) {
            return Ok(Transformation::ReplaceWithNop);
        }
        if is_primitive_boxing_insn(insn) || is_primitive_type_conversion(insn) {
            let input = self.frames[source]
                .as_ref()
                .ok_or_else(|| error("a dead instruction used by a live one"))?
                .stack
                .last()
                .ok_or_else(|| error("a coercion with no input"))?;
            return match input.size {
                1 => Ok(Transformation::ReplaceWithPop1),
                2 => Ok(Transformation::ReplaceWithPop2),
                _ => Err(error("an unexpected popped value size")),
            };
        }
        match size {
            1 => Ok(Transformation::InsertPop1After),
            2 => Ok(Transformation::InsertPop2After),
            _ => Err(error("an unexpected popped value size")),
        }
    }

    /// The second walk: every `pop` whose pushers are all free, with its pushers.
    fn transformations(&self) -> Result<BTreeMap<usize, Transformation>, AnalyzerError> {
        let mut transformations = BTreeMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            let (Node::Insn(Insn::Op(POP)), Some(frame)) = (node, &self.frames[index]) else {
                continue;
            };
            let Some(top) = frame.stack.last() else {
                continue;
            };
            if top.insns.iter().any(|&source| self.should_keep(source)) {
                continue;
            }
            transformations.insert(index, Transformation::ReplaceWithNop);
            for &source in top.insns.iter() {
                if let Entry::Vacant(entry) = transformations.entry(source) {
                    entry.insert(self.combine_with_pop(source, top.size)?);
                }
            }
        }
        Ok(transformations)
    }
}

/// Remove the `pop`s of `method`, a member of `owner`, whose values need not be pushed, with the
/// pushing where it is pure; `true` when anything changed.
pub(crate) fn propagate(method: &mut MethodNode, owner: &str) -> Result<bool, AnalyzerError> {
    if !method
        .instructions()
        .any(|insn| is_pop(insn) || is_pure_push(insn))
    {
        return Ok(false);
    }
    let mut interpreter = HazardsTracking::new(method.nodes.len());
    // The frames, and the instructions marked untouchable, are the same in any visiting order; the
    // index order only spares revisiting the rest of the method after every joining branch.
    let frames = analyze_with(
        method,
        owner,
        &mut interpreter,
        &mut PlainFrames,
        AnalyzerOptions {
            in_index_order: true,
            ..AnalyzerOptions::default()
        },
    )?;
    let mut run = Run {
        nodes: &method.nodes,
        frames: &frames,
        dont_touch: interpreter.dont_touch,
    };
    run.mark_hazards();
    let transformations = run.transformations()?;
    if transformations.is_empty() {
        return Ok(false);
    }
    let op = |code: u8| Node::Insn(Insn::Op(code));
    let mut nodes = Vec::with_capacity(method.nodes.len() + transformations.len());
    for (index, node) in std::mem::take(&mut method.nodes).into_iter().enumerate() {
        match transformations.get(&index) {
            None => nodes.push(node),
            Some(Transformation::ReplaceWithNop) => nodes.push(op(NOP)),
            Some(Transformation::ReplaceWithPop1) => nodes.push(op(POP)),
            Some(Transformation::ReplaceWithPop2) => nodes.push(op(POP2)),
            Some(Transformation::InsertPop1After) => nodes.extend([node, op(POP)]),
            Some(Transformation::InsertPop2After) => nodes.extend([node, op(POP2)]),
        }
    }
    method.nodes = nodes;
    Ok(true)
}
