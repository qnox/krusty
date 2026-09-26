//! kotlinc's `ConstantConditionEliminationMethodTransformer` over a [`MethodNode`]: an `int`
//! comparison whose outcome the constants reaching it decide becomes a `goto` or goes.
//!
//! A method is looked at only when it has both an `int` jump (`ifeq`…`ifle`,
//! `if_icmpeq`…`if_icmple`) and an `int` constant. Each round runs the constant-propagation
//! analysis ([`interpreter`]) and, at once:
//!
//! - an `if<cond>` on a known `int` gets a `pop` before it and becomes a `goto` to its target when
//!   the value satisfies the condition, or goes when it does not;
//! - an `if_icmp<cond>` on two known `int`s gets two `pop`s before it and becomes a `goto` or goes
//!   the same way;
//! - an `if_icmp<cond>` whose second operand is a known `0` gets a `pop` before it and becomes the
//!   `if<cond>` of its first operand;
//! - every node the analysis does not reach goes, except a label (line numbers go with the code).
//!
//! The rounds repeat while a round changes anything, since a jump become a `goto` leaves code
//! unreachable and can make a merge see one value fewer. The popped operands stay for the later
//! passes, which remove a load or constant that is only popped.

mod interpreter;

#[cfg(test)]
mod tests;

use super::analysis::{analyze, opcode, AnalyzerError, Frame};
use super::opcodes::*;
use crate::jvm::method_node::{Insn, MethodNode, Node};
use interpreter::{int_constant, ConstValue, ConstantPropagationInterpreter};

const IFLT: u8 = 0x9b;
const IFGE: u8 = 0x9c;
const IFGT: u8 = 0x9d;
const IF_ICMPLT: u8 = 0xa1;
const IF_ICMPGE: u8 = 0xa2;
const IF_ICMPGT: u8 = 0xa3;
const IF_ICMPLE: u8 = 0xa4;

/// What one round does to a jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rewrite {
    /// Pop this many known operands, then jump unconditionally (`true`) or fall through.
    Decided { pops: usize, jumps: bool },
    /// Pop the known `0`, then compare the first operand with zero by this `if<cond>`.
    WithZero(u8),
}

/// Whether `value` satisfies the `if<cond>` `op` (`IFEQ`…`IFLE`).
fn holds_against_zero(op: u8, value: i32) -> bool {
    match op {
        IFEQ => value == 0,
        IFNE => value != 0,
        IFLT => value < 0,
        IFGE => value >= 0,
        IFGT => value > 0,
        _ => value <= 0,
    }
}

/// Whether `first <cond> second` holds for the `if_icmp<cond>` `op` (`IF_ICMPEQ`…`IF_ICMPLE`).
fn holds(op: u8, first: i32, second: i32) -> bool {
    match op {
        IF_ICMPEQ => first == second,
        IF_ICMPNE => first != second,
        IF_ICMPLT => first < second,
        IF_ICMPGE => first >= second,
        IF_ICMPGT => first > second,
        _ => first <= second,
    }
}

/// The `if<cond>` comparing with zero as the `if_icmp<cond>` `op` compares with its second operand.
fn against_zero(op: u8) -> u8 {
    op - IF_ICMPEQ + IFEQ
}

fn is_zero_jump(op: u8) -> bool {
    (IFEQ..=IFLE).contains(&op)
}

fn is_binary_jump(op: u8) -> bool {
    (IF_ICMPEQ..=IF_ICMPLE).contains(&op)
}

/// kotlinc's `hasOptimizableConditions`: an `int` jump and an `int` constant.
fn has_optimizable_conditions(method: &MethodNode) -> bool {
    method.instructions().any(|insn| {
        let op = opcode(insn);
        matches!(insn, Insn::Jump { .. }) && (is_zero_jump(op) || is_binary_jump(op))
    }) && method
        .instructions()
        .any(|insn| int_constant(insn).is_some())
}

/// The value `depth` entries below the top of `frame`'s stack.
fn peek(frame: &Frame<ConstValue>, depth: usize) -> Option<&ConstValue> {
    frame.stack.iter().rev().nth(depth)
}

/// What the jump `op` becomes before `frame`, if the constants decide anything.
fn rewrite(op: u8, frame: &Frame<ConstValue>) -> Option<Rewrite> {
    if is_zero_jump(op) {
        let value = peek(frame, 0)?.known()?;
        return Some(Rewrite::Decided {
            pops: 1,
            jumps: holds_against_zero(op, value),
        });
    }
    if !is_binary_jump(op) {
        return None;
    }
    let first = peek(frame, 1)?.known();
    let second = peek(frame, 0)?.known();
    match (first, second) {
        (Some(first), Some(second)) => Some(Rewrite::Decided {
            pops: 2,
            jumps: holds(op, first, second),
        }),
        (_, Some(0)) => Some(Rewrite::WithZero(against_zero(op))),
        _ => None,
    }
}

/// One round (`ConstantConditionsOptimization.run`); `true` when it changed the method.
fn round(method: &mut MethodNode, owner: &str) -> Result<bool, AnalyzerError> {
    let frames = analyze(method, owner, &mut ConstantPropagationInterpreter)?;
    let mut changed = false;
    let mut nodes = Vec::with_capacity(method.nodes.len());
    for (node, frame) in std::mem::take(&mut method.nodes).into_iter().zip(&frames) {
        let Some(frame) = frame else {
            if matches!(node, Node::Label(_)) {
                nodes.push(node);
            } else {
                changed = true;
            }
            continue;
        };
        let Node::Insn(Insn::Jump { op, target }) = node else {
            nodes.push(node);
            continue;
        };
        let Some(action) = rewrite(op, frame) else {
            nodes.push(node);
            continue;
        };
        changed = true;
        match action {
            Rewrite::Decided { pops, jumps } => {
                nodes.extend((0..pops).map(|_| Node::Insn(Insn::Op(POP))));
                if jumps {
                    nodes.push(Node::Insn(Insn::Jump { op: GOTO, target }));
                }
            }
            Rewrite::WithZero(op) => {
                nodes.push(Node::Insn(Insn::Op(POP)));
                nodes.push(Node::Insn(Insn::Jump { op, target }));
            }
        }
    }
    method.nodes = nodes;
    Ok(changed)
}

/// Fold every `int` jump of `method`, a member of `owner`, whose outcome its constant operands
/// decide; `true` when anything changed.
pub(crate) fn eliminate(method: &mut MethodNode, owner: &str) -> Result<bool, AnalyzerError> {
    if !has_optimizable_conditions(method) {
        return Ok(false);
    }
    let mut changed = false;
    while round(method, owner)? {
        changed = true;
    }
    Ok(changed)
}
