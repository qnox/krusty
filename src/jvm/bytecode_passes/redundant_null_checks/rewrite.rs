//! kotlinc's `transformTrivialChecks`: each check whose operand the analysis knows is rewritten,
//! in instruction order, on the method as it stands after the checks before it were rewritten.
//!
//! - `ifnull`/`ifnonnull` of a value known `null` or non-null becomes a `goto` when it always
//!   jumps and goes when it never does;
//! - `instanceof` of `null` is `false`, and of a non-null value of exactly the tested class
//!   `true` — unless it is a reified placeholder;
//! - `checkNotNull*` of a non-null value goes, with the load and message feeding it.
//!
//! The operand a jump or `instanceof` no longer takes is dropped where it was made: the `aload`,
//! `dup` or `aconst_null` right before goes, and anything else is popped.

use super::super::analysis::opcode;
use super::super::descriptors;
use super::super::opcodes::*;
use super::super::redundant_checkcasts::is_reified_marker;
use super::assumptions::Listing;
use super::nullability::{NullValue, Nullability};
use super::Check;
use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

/// The method under rewrite, with where each node came from.
pub(super) struct Rewrite<'a> {
    pub(super) method: &'a mut MethodNode,
    /// Per node, the position of the node in the method as the pass received it; `None` for a node
    /// the pass added.
    pub(super) origins: &'a mut Vec<Option<usize>>,
    /// Per node, its position when this round of the pass started.
    round: Vec<Option<usize>>,
    listing: Listing,
    /// Whether a jump or `instanceof` was rewritten: kotlinc's `changes`, which runs another round.
    pub(super) changes: bool,
    /// Whether anything was rewritten.
    pub(super) edited: bool,
}

impl<'a> Rewrite<'a> {
    pub(super) fn new(
        method: &'a mut MethodNode,
        origins: &'a mut Vec<Option<usize>>,
        listing: Listing,
    ) -> Self {
        let round = (0..method.nodes.len()).map(Some).collect();
        Rewrite {
            method,
            origins,
            round,
            listing,
            changes: false,
            edited: false,
        }
    }

    /// Where the node that stood at `at` when the round started stands now.
    fn position(&self, at: usize) -> Option<usize> {
        self.round.iter().position(|&from| from == Some(at))
    }

    fn previous(&self, at: usize) -> Option<usize> {
        self.listing.previous(&self.method.nodes, at)
    }

    fn opcode_at(&self, at: Option<usize>) -> Option<u8> {
        match self.method.nodes.get(at?)? {
            Node::Insn(insn) => Some(opcode(insn)),
            _ => None,
        }
    }

    fn remove(&mut self, at: usize) {
        self.method.nodes.remove(at);
        self.origins.remove(at);
        self.round.remove(at);
        self.edited = true;
    }

    fn insert_before(&mut self, at: usize, insn: Insn) {
        self.method.nodes.insert(at, Node::Insn(insn));
        self.origins.insert(at, None);
        self.round.insert(at, None);
        self.edited = true;
    }

    fn set(&mut self, at: usize, insn: Insn) {
        self.method.nodes[at] = Node::Insn(insn);
        self.edited = true;
    }

    /// `popReferenceValueBefore`; returns where the instruction at `at` stands afterwards.
    fn pop_reference_value_before(&mut self, at: usize) -> usize {
        let previous = self.previous(at);
        match (self.opcode_at(previous), previous) {
            (Some(ACONST_NULL | DUP | ALOAD), Some(previous)) => {
                self.remove(previous);
                at - 1
            }
            _ => {
                self.insert_before(at, Insn::Op(POP));
                at + 1
            }
        }
    }

    /// Rewrite each check the analysis knows the operand of, in order (`transformTrivialChecks`).
    pub(super) fn apply(&mut self, known: &[(usize, NullValue)]) {
        for (id, value) in known {
            let id = *id;
            let Some(at) = self.position(id) else {
                continue;
            };
            let Node::Insn(insn) = &self.method.nodes[at] else {
                continue;
            };
            let nullability = value.nullability();
            match insn.clone() {
                Insn::Jump { op: IFNULL, target } => {
                    self.trivial_null_jump(at, target, nullability == Nullability::Null)
                }
                Insn::Jump {
                    op: IFNONNULL,
                    target,
                } => self.trivial_null_jump(at, target, nullability == Nullability::NotNull),
                Insn::Type {
                    op: INSTANCEOF,
                    class,
                } => self.instance_of(at, &class, value),
                other => match Check::of(&other) {
                    _ if nullability != Nullability::NotNull => {}
                    Some(Check::NotNull) => self.trivial_check_not_null(at),
                    Some(Check::NotNullWithMessage) => self.trivial_check_with_message(at),
                    Some(Check::ExpressionValue) => self.trivial_expression_check(id),
                    Some(Check::Parameter) | None => {}
                },
            }
        }
    }

    /// `transformTrivialNullJump`.
    fn trivial_null_jump(&mut self, at: usize, target: LabelId, always: bool) {
        self.changes = true;
        let at = self.pop_reference_value_before(at);
        if always {
            self.set(at, Insn::Jump { op: GOTO, target });
        } else {
            self.remove(at);
        }
    }

    /// `transformInstanceOf`.
    fn instance_of(&mut self, at: usize, class: &str, value: &NullValue) {
        let previous = self
            .previous(at)
            .map(|previous| &self.method.nodes[previous]);
        if matches!(previous, Some(Node::Insn(insn)) if is_reified_marker(insn)) {
            return;
        }
        let result = match value.nullability() {
            Nullability::Null => ICONST_0,
            Nullability::NotNull
                if value
                    .descriptor()
                    .is_some_and(|descriptor| descriptors::internal_name(descriptor) == class) =>
            {
                ICONST_1
            }
            _ => return,
        };
        self.changes = true;
        let at = self.pop_reference_value_before(at);
        self.set(at, Insn::Op(result));
    }

    /// `transformTrivialCheckNotNull`: `dup|aload; checkNotNull(Object)`.
    fn trivial_check_not_null(&mut self, at: usize) {
        let Some(feed) = self.previous(at) else {
            return;
        };
        if !matches!(self.opcode_at(Some(feed)), Some(DUP | ALOAD)) {
            return;
        }
        self.remove(at);
        self.remove(feed);
    }

    /// `transformTrivialCheckNotNullWithMessage`: `dup|aload; ldc; checkNotNull(Object, String)`.
    fn trivial_check_with_message(&mut self, at: usize) {
        let Some(message) = self.previous(at) else {
            return;
        };
        if self.opcode_at(Some(message)) != Some(LDC) {
            return;
        }
        let Some(feed) = self.previous(message) else {
            return;
        };
        if !matches!(self.opcode_at(Some(feed)), Some(DUP | ALOAD)) {
            return;
        }
        self.remove(at);
        self.remove(message);
        self.remove(feed);
    }

    /// `transformTrivialCheckExpressionValueIsNotNull`: `ldc; checkNotNullExpressionValue`, with the
    /// checked value dropped before the message.
    fn trivial_expression_check(&mut self, id: usize) {
        let Some(at) = self.position(id) else {
            return;
        };
        let Some(message) = self.previous(at) else {
            return;
        };
        if self.opcode_at(Some(message)) != Some(LDC) {
            return;
        }
        let message = self.pop_reference_value_before(message);
        let at = self.position(id).expect("the check is still in the method");
        self.remove(at);
        self.remove(message);
    }
}
