//! kotlinc 2.5's `tryRewriteGotoToComparison`: a `goto` that carries a known `int` to an
//! `if<cond>` goes straight to the branch that value takes.
//!
//! The `goto`'s target is followed through labels, line numbers, `nop`s and further `goto`s to the
//! first other instruction. When that is an `if<cond>` comparing with zero, in the same inline
//! scope as the `goto`, the `goto` gets a `pop` before it and jumps to the `if`'s target when the
//! value satisfies the condition, or to the label right after the `if` (made when there is none)
//! when it does not. When the `if` is on another line than the `goto`, that line is marked before
//! the `goto`, so the jump keeps the line the comparison had.
//!
//! kotlinc rewrites each `goto` as its round reaches it, so the body it looks through is the one
//! the round has made so far, followed by the nodes it has not reached yet: [`Body`].

use super::super::coroutines::markers::is_fake_local_variable_for_inline;
use super::super::opcodes::*;
use super::{holds_against_zero, is_zero_jump};
use crate::jvm::method_node::{Insn, LabelId, LocalVariable, MethodNode, Node};
use std::collections::HashSet;

/// A round's body as it stands when it reaches a `goto`: the nodes it has rewritten, the `goto`
/// last, then the nodes it has yet to reach. A node a rewrite placed among the latter has no
/// original position.
pub(super) struct Body<'a> {
    pub(super) done: &'a mut Vec<Node>,
    pub(super) ahead: &'a mut Vec<(Option<usize>, Option<Node>)>,
    pub(super) from: usize,
}

impl Body<'_> {
    fn len(&self) -> usize {
        self.done.len() + self.ahead.len() - self.from
    }

    fn get(&self, at: usize) -> &Node {
        match at.checked_sub(self.done.len()) {
            None => &self.done[at],
            Some(ahead) => self.ahead[self.from + ahead]
                .1
                .as_ref()
                .expect("a node not reached yet is still in place"),
        }
    }

    fn insert(&mut self, at: usize, node: Node) {
        match at.checked_sub(self.done.len()) {
            Some(ahead) => self.ahead.insert(self.from + ahead, (None, Some(node))),
            None => self.done.insert(at, node),
        }
    }

    /// The position of `label`, if it is placed.
    fn position(&self, label: LabelId) -> Option<usize> {
        (0..self.len()).find(|&at| matches!(self.get(at), Node::Label(placed) if *placed == label))
    }
}

/// Thread the `goto` that ends `body.done`, which carries `value`; `true` when it changed.
/// `method` supplies the new labels and the inline-scope markers.
pub(super) fn thread(method: &mut MethodNode, mut body: Body<'_>, value: i32) -> bool {
    let at = body.done.len() - 1;
    let Node::Insn(Insn::Jump { op: GOTO, target }) = *body.get(at) else {
        return false;
    };
    let Some(compared) = executable_target(&body, target) else {
        return false;
    };
    let Node::Insn(Insn::Jump { op, target: branch }) = *body.get(compared) else {
        return false;
    };
    let locals = &method.local_variables;
    if !is_zero_jump(op) || inline_scope(locals, &body, at) != inline_scope(locals, &body, compared)
    {
        return false;
    }
    let goto = body.done.pop().expect("the goto ends the rewritten nodes");
    let (compared_line, goto_line) = (line_at(&body, compared), line_at(&body, at));
    body.done.push(Node::Insn(Insn::Op(POP)));
    let mut shift = 1;
    if let Some(line) = compared_line.filter(|&line| Some(line) != goto_line) {
        let start = method.new_label();
        body.done
            .extend([Node::Label(start), Node::Line { line, start }]);
        shift += 2;
    }
    let compared = if compared > at {
        compared + shift
    } else {
        compared
    };
    body.done.push(goto);
    let new_target = if holds_against_zero(op, value) {
        branch
    } else {
        label_after(method, &mut body, compared)
    };
    *body.done.last_mut().expect("the goto is back in place") = Node::Insn(Insn::Jump {
        op: GOTO,
        target: new_target,
    });
    true
}

/// kotlinc's `targetExecutableInsn`: the first instruction reached from `label` that is not a
/// label, a line number or a `nop`, following `goto`s; `None` for a `goto` cycle.
fn executable_target(body: &Body<'_>, label: LabelId) -> Option<usize> {
    let mut at = body.position(label)?;
    let mut visited = HashSet::new();
    while at < body.len() {
        match body.get(at) {
            Node::Insn(Insn::Jump { op: GOTO, target }) => {
                if !visited.insert(at) {
                    return None;
                }
                at = body.position(*target)?;
            }
            Node::Label(_) | Node::Line { .. } | Node::Insn(Insn::Op(NOP)) => at += 1,
            Node::Insn(_) => return Some(at),
        }
    }
    None
}

/// kotlinc's `getLineNumberOrNull`: the nearest line number at or before `at`.
fn line_at(body: &Body<'_>, at: usize) -> Option<u16> {
    (0..=at).rev().find_map(|at| match body.get(at) {
        Node::Line { line, .. } => Some(*line),
        _ => None,
    })
}

/// kotlinc's `getOrCreateLabelAfter`: the label that follows `at` past `nop`s and line numbers, or
/// a new one placed right after it.
fn label_after(method: &mut MethodNode, body: &mut Body<'_>, at: usize) -> LabelId {
    let following = (at + 1..body.len())
        .map(|at| body.get(at))
        .find(|node| !matches!(node, Node::Line { .. } | Node::Insn(Insn::Op(NOP))));
    if let Some(Node::Label(label)) = following {
        return *label;
    }
    let label = method.new_label();
    body.insert(at + 1, Node::Label(label));
    label
}

/// kotlinc's `computeInlineScopes`: the innermost `$i$f$`/`$i$a$` marker whose range holds the node
/// at `at`, as an index into `locals`. Markers open widest first, so the narrowest of those opening
/// at one label is innermost.
fn inline_scope(locals: &[LocalVariable], body: &Body<'_>, at: usize) -> Option<usize> {
    let mut markers: Vec<usize> = (0..locals.len())
        .filter(|&index| is_fake_local_variable_for_inline(&locals[index].name))
        .collect();
    if markers.is_empty() {
        return None;
    }
    let span = |index: usize| {
        let start = body.position(locals[index].start).unwrap_or(0);
        let end = body.position(locals[index].end).unwrap_or(0);
        end.saturating_sub(start)
    };
    markers.sort_by_key(|&index| std::cmp::Reverse(span(index)));
    let mut active: Vec<usize> = Vec::new();
    for position in 0..=at {
        if let Node::Label(label) = body.get(position) {
            active.retain(|&index| locals[index].end != *label);
            active.extend(
                markers
                    .iter()
                    .copied()
                    .filter(|&index| locals[index].start == *label),
            );
        }
    }
    active.last().copied()
}
