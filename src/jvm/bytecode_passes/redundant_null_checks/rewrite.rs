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
use crate::jvm::method_node::{Insn, LabelId, Node};

/// What the round does to the node at one position of the method as the round received it.
#[derive(Clone, Default)]
struct Edit {
    /// The node goes.
    removed: bool,
    /// The instruction that takes the node's place.
    replacement: Option<Insn>,
    /// How many `pop`s the round puts right before the node.
    pops_before: usize,
}

/// A node of the method as the round has rewritten it so far.
#[derive(Clone, Copy)]
enum Current {
    /// The node at this position of the method as the round received it.
    Node(usize),
    /// A `pop` the round added.
    AddedPop,
}

/// The round's rewrite of a method, kept as an edit per node of the method as the round received
/// it. kotlinc rewrites its node list in place; recording the same edits against the unchanged
/// positions keeps every check where the analysis found it, so each lookup is direct and the round
/// builds the new node list once, in one pass.
pub(super) struct Rewrite<'a> {
    nodes: &'a [Node],
    edits: Vec<Edit>,
    listing: Listing,
    /// Whether a jump or `instanceof` was rewritten: kotlinc's `changes`, which runs another round.
    pub(super) changes: bool,
    /// Whether anything was rewritten.
    pub(super) edited: bool,
}

impl<'a> Rewrite<'a> {
    pub(super) fn new(nodes: &'a [Node], listing: Listing) -> Self {
        Rewrite {
            nodes,
            edits: vec![Edit::default(); nodes.len()],
            listing,
            changes: false,
            edited: false,
        }
    }

    /// The node list the round made, and for each node the entry of `origins` (the position in the
    /// method the pass received) of the node it was; `None` for a `pop` the pass added.
    pub(super) fn finish(self, origins: &[Option<usize>]) -> (Vec<Node>, Vec<Option<usize>>) {
        let mut nodes = Vec::with_capacity(self.nodes.len());
        let mut from = Vec::with_capacity(self.nodes.len());
        for (at, (node, edit)) in self.nodes.iter().zip(self.edits).enumerate() {
            for _ in 0..edit.pops_before {
                nodes.push(Node::Insn(Insn::Op(POP)));
                from.push(None);
            }
            if edit.removed {
                continue;
            }
            nodes.push(edit.replacement.map_or_else(|| node.clone(), Node::Insn));
            from.push(origins[at]);
        }
        (nodes, from)
    }

    /// The instruction at `at` as the round has left it; `None` for a node that is not one.
    fn insn(&self, at: usize) -> Option<&Insn> {
        match (&self.edits[at].replacement, &self.nodes[at]) {
            (Some(insn), _) | (None, Node::Insn(insn)) => Some(insn),
            _ => None,
        }
    }

    /// `getPrevious` of the node at `at` in the method as the round has rewritten it so far: an
    /// added `pop` right before it, or the nearest earlier node still in ASM's list.
    fn previous(&self, at: usize) -> Option<Current> {
        if self.edits[at].pops_before > 0 {
            return Some(Current::AddedPop);
        }
        for index in (0..at).rev() {
            let edit = &self.edits[index];
            if !edit.removed && self.listing.holds(&self.nodes[index]) {
                return Some(Current::Node(index));
            }
            if edit.pops_before > 0 {
                return Some(Current::AddedPop);
            }
        }
        None
    }

    fn remove(&mut self, at: usize) {
        self.edits[at].removed = true;
        self.edited = true;
    }

    fn set(&mut self, at: usize, insn: Insn) {
        self.edits[at].replacement = Some(insn);
        self.edited = true;
    }

    /// `popReferenceValueBefore`.
    fn pop_reference_value_before(&mut self, at: usize) {
        match self.previous(at) {
            Some(Current::Node(previous))
                if matches!(
                    self.insn(previous).map(opcode),
                    Some(ACONST_NULL | DUP | ALOAD)
                ) =>
            {
                self.remove(previous)
            }
            _ => {
                self.edits[at].pops_before += 1;
                self.edited = true;
            }
        }
    }

    /// The node right before `at` when it is an instruction of the round's method with an opcode
    /// in `ops`.
    fn previous_of(&self, at: usize, ops: &[u8]) -> Option<usize> {
        match self.previous(at)? {
            Current::Node(previous)
                if self
                    .insn(previous)
                    .is_some_and(|insn| ops.contains(&opcode(insn))) =>
            {
                Some(previous)
            }
            _ => None,
        }
    }

    /// Rewrite each check the analysis knows the operand of, in order (`transformTrivialChecks`).
    pub(super) fn apply(&mut self, known: &[(usize, NullValue)]) {
        for (at, value) in known {
            let at = *at;
            if self.edits[at].removed {
                continue;
            }
            let Some(insn) = self.insn(at).cloned() else {
                continue;
            };
            let nullability = value.nullability();
            match insn {
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
                    Some(Check::ExpressionValue) => self.trivial_expression_check(at),
                    Some(Check::Parameter) | None => {}
                },
            }
        }
    }

    /// `transformTrivialNullJump`.
    fn trivial_null_jump(&mut self, at: usize, target: LabelId, always: bool) {
        self.changes = true;
        self.pop_reference_value_before(at);
        if always {
            self.set(at, Insn::Jump { op: GOTO, target });
        } else {
            self.remove(at);
        }
    }

    /// `transformInstanceOf`.
    fn instance_of(&mut self, at: usize, class: &str, value: &NullValue) {
        if let Some(Current::Node(previous)) = self.previous(at) {
            if self.insn(previous).is_some_and(is_reified_marker) {
                return;
            }
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
        self.pop_reference_value_before(at);
        self.set(at, Insn::Op(result));
    }

    /// `transformTrivialCheckNotNull`: `dup|aload; checkNotNull(Object)`.
    fn trivial_check_not_null(&mut self, at: usize) {
        let Some(feed) = self.previous_of(at, &[DUP, ALOAD]) else {
            return;
        };
        self.remove(at);
        self.remove(feed);
    }

    /// `transformTrivialCheckNotNullWithMessage`: `dup|aload; ldc; checkNotNull(Object, String)`.
    fn trivial_check_with_message(&mut self, at: usize) {
        let Some(message) = self.previous_of(at, &[LDC]) else {
            return;
        };
        let Some(feed) = self.previous_of(message, &[DUP, ALOAD]) else {
            return;
        };
        self.remove(at);
        self.remove(message);
        self.remove(feed);
    }

    /// `transformTrivialCheckExpressionValueIsNotNull`: `ldc; checkNotNullExpressionValue`, with the
    /// checked value dropped before the message.
    fn trivial_expression_check(&mut self, at: usize) {
        let Some(message) = self.previous_of(at, &[LDC]) else {
            return;
        };
        self.pop_reference_value_before(message);
        self.remove(at);
        self.remove(message);
    }
}
