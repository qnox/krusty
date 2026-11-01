//! Statements and expressions from a decoded body's operations, in structured order.
//!
//! Each decoded value becomes an expression where it is consumed. A pure value (a constant, a
//! parameter or local read, arithmetic that cannot throw) is built in place; a value with an
//! effect is evaluated where the body evaluated it, into a temporary when something later reads
//! it. Writing a local first saves every pending read of that local into a temporary, so moving a
//! read to its use never observes a later write. Block parameters become locals the incoming edges
//! assign.

use super::super::JvmLibraries;
use super::calls::{lambda_parameter, primitive_box, CallForm};
use super::control::{Exit, Shape, Transfer};
use crate::fx_hash::HashMap;
use crate::jvm::inline_template::{
    BinaryOp, BlockId, Comparison, Condition, DecodedBody, Edge, InvokeKind, JvmType, LocalId,
    OpKind, Terminator, ValueId,
};
use crate::jvm::method_node::{Category, Constant};
use crate::libraries::{
    DependencyInlineTemplate, LibraryCallable, TemplateBinaryOp, TemplateCallee,
    TemplateComparison, TemplateCondition, TemplateDecline, TemplateExpr, TemplateExprId,
    TemplateExprKind, TemplateLabel, TemplateLocal, TemplateLocalId, TemplateStmt, TemplateStmtId,
    TemplateStmtKind,
};
use crate::types::{type_name, Ty, TyKind};

/// What a decoded value stands for until it is consumed.
#[derive(Clone)]
enum Slot {
    Expr {
        expr: TemplateExprId,
        /// The template locals the expression reads.
        reads: Vec<TemplateLocalId>,
    },
    /// A `new` instance its constructor call has not initialized yet.
    New,
    /// An `lcmp`/`fcmp*`/`dcmp*` result, which only a following branch consumes.
    Comparison {
        op: BinaryOp,
        lhs: ValueId,
        rhs: ValueId,
    },
}

fn unsupported(reason: &'static str) -> TemplateDecline {
    TemplateDecline::Unsupported(reason)
}

fn primitive_type(ty: &JvmType) -> Option<Ty> {
    Some(match ty {
        JvmType::Boolean => Ty::Boolean,
        JvmType::Byte => Ty::Byte,
        JvmType::Char => Ty::Char,
        JvmType::Short => Ty::Short,
        JvmType::Int => Ty::Int,
        JvmType::Long => Ty::Long,
        JvmType::Float => Ty::Float,
        JvmType::Double => Ty::Double,
        JvmType::Reference(_) | JvmType::Null => return None,
    })
}

fn category_type(category: Category) -> Ty {
    match category {
        Category::Int => Ty::Int,
        Category::Long => Ty::Long,
        Category::Float => Ty::Float,
        Category::Double => Ty::Double,
        Category::Reference => Ty::Error,
    }
}

fn is_int_like(ty: Ty) -> bool {
    [Ty::Boolean, Ty::Byte, Ty::Char, Ty::Short, Ty::Int].contains(&ty)
}

fn comparison(comparison: Comparison) -> TemplateComparison {
    match comparison {
        Comparison::Eq => TemplateComparison::Eq,
        Comparison::Ne => TemplateComparison::Ne,
        Comparison::Lt => TemplateComparison::Lt,
        Comparison::Ge => TemplateComparison::Ge,
        Comparison::Gt => TemplateComparison::Gt,
        Comparison::Le => TemplateComparison::Le,
    }
}

/// The comparison that holds exactly when `comparison` does not, NaN aside.
fn complement(comparison: TemplateComparison) -> TemplateComparison {
    match comparison {
        TemplateComparison::Eq => TemplateComparison::Ne,
        TemplateComparison::Ne => TemplateComparison::Eq,
        TemplateComparison::Lt => TemplateComparison::Ge,
        TemplateComparison::Ge => TemplateComparison::Lt,
        TemplateComparison::Gt => TemplateComparison::Le,
        TemplateComparison::Le => TemplateComparison::Gt,
    }
}

/// The values each operation, terminator and edge reads.
fn operands(kind: &OpKind) -> Vec<ValueId> {
    match kind {
        OpKind::Null | OpKind::Constant(_) | OpKind::LoadLocal(_) | OpKind::New { .. } => {
            Vec::new()
        }
        OpKind::IncrementLocal { .. } | OpKind::GetField { object: None, .. } => Vec::new(),
        OpKind::StoreLocal { value, .. }
        | OpKind::Negate { operand: value, .. }
        | OpKind::Convert { operand: value, .. }
        | OpKind::GetField {
            object: Some(value),
            ..
        }
        | OpKind::PutField {
            object: None,
            value,
            ..
        }
        | OpKind::NewArray { length: value, .. }
        | OpKind::ArrayLength(value)
        | OpKind::CheckCast { value, .. }
        | OpKind::InstanceOf { value, .. }
        | OpKind::MonitorEnter(value)
        | OpKind::MonitorExit(value) => vec![*value],
        OpKind::Binary { lhs, rhs, .. } => vec![*lhs, *rhs],
        OpKind::PutField {
            object: Some(object),
            value,
            ..
        } => vec![*object, *value],
        OpKind::Invoke { arguments, .. } | OpKind::InvokeDynamic { arguments, .. } => {
            arguments.clone()
        }
        OpKind::MultiNewArray { dimensions, .. } => dimensions.clone(),
        OpKind::ArrayLoad { array, index, .. } => vec![*array, *index],
        OpKind::ArrayStore {
            array,
            index,
            value,
            ..
        } => vec![*array, *index, *value],
    }
}

fn terminator_operands(terminator: &Terminator) -> Vec<ValueId> {
    let edge = |edge: &Edge| edge.arguments.clone();
    match terminator {
        Terminator::Goto(target) => edge(target),
        Terminator::Branch {
            condition,
            taken,
            not_taken,
        } => {
            let mut values = match *condition {
                Condition::IntZero { value, .. } | Condition::Null { value, .. } => vec![value],
                Condition::Ints { lhs, rhs, .. } | Condition::References { lhs, rhs, .. } => {
                    vec![lhs, rhs]
                }
            };
            values.extend(edge(taken));
            values.extend(edge(not_taken));
            values
        }
        Terminator::Switch {
            value,
            cases,
            default,
        } => {
            let mut values = vec![*value];
            values.extend(cases.iter().flat_map(|(_, target)| edge(target)));
            values.extend(edge(default));
            values
        }
        Terminator::Return(value) => value.iter().copied().collect(),
        Terminator::Throw(value) => vec![*value],
    }
}

pub(super) struct Emitter<'a> {
    libraries: &'a JvmLibraries,
    callable: &'a LibraryCallable,
    body: &'a DecodedBody,
    /// The declaration parameter each decoded parameter local holds.
    parameter_of: Vec<Option<usize>>,
    /// Whether some operation reads each decoded local.
    read: Vec<bool>,
    /// The template local of each decoded local, from its first write.
    local_of: Vec<Option<TemplateLocalId>>,
    block_parameters: HashMap<(BlockId, usize), TemplateLocalId>,
    /// Reads of each value not consumed yet.
    uses: Vec<u32>,
    slots: Vec<Option<Slot>>,
    /// The values the current block defined, for saving pending reads of a written local.
    defined: Vec<ValueId>,
    template: DependencyInlineTemplate,
    line: Option<u16>,
    /// A value whose effectful expression waits to be built into its single reader. Emitting any
    /// statement or another effect first saves it to a temporary, so effects keep their order.
    pending: Option<ValueId>,
    /// Whether the operation being translated consumed the pending value: the value it defines
    /// then carries that effect and becomes the pending one.
    absorbed: bool,
}

impl<'a> Emitter<'a> {
    pub(super) fn new(
        libraries: &'a JvmLibraries,
        callable: &'a LibraryCallable,
        body: &'a DecodedBody,
    ) -> Emitter<'a> {
        let mut parameter_of = vec![None; body.locals.len()];
        for (ordinal, local) in body.parameters.iter().enumerate() {
            parameter_of[local.0 as usize] = Some(ordinal);
        }
        let mut read = vec![false; body.locals.len()];
        let mut uses = vec![0u32; body.values.len()];
        for block in &body.blocks {
            for op in &block.ops {
                if let OpKind::LoadLocal(local) | OpKind::IncrementLocal { local, .. } = op.kind {
                    read[local.0 as usize] = true;
                }
                for value in operands(&op.kind) {
                    uses[value.0 as usize] += 1;
                }
            }
            for value in terminator_operands(&block.terminator) {
                uses[value.0 as usize] += 1;
            }
        }
        Emitter {
            libraries,
            callable,
            body,
            parameter_of,
            read,
            local_of: vec![None; body.locals.len()],
            block_parameters: HashMap::default(),
            uses,
            slots: vec![None; body.values.len()],
            defined: Vec::new(),
            template: DependencyInlineTemplate {
                locals: Vec::new(),
                expressions: Vec::new(),
                statements: Vec::new(),
                body: Vec::new(),
            },
            line: None,
            pending: None,
            absorbed: false,
        }
    }

    pub(super) fn finish(
        mut self,
        shapes: &[Shape],
    ) -> Result<DependencyInlineTemplate, TemplateDecline> {
        let body = self.shapes(shapes)?;
        self.template.body = body;
        Ok(self.template)
    }

    fn new_local(&mut self, ty: Ty) -> TemplateLocalId {
        self.template.locals.push(TemplateLocal { ty });
        TemplateLocalId(self.template.locals.len() as u32 - 1)
    }

    fn expr(&mut self, kind: TemplateExprKind, ty: Ty) -> TemplateExprId {
        self.template.expressions.push(TemplateExpr { kind, ty });
        TemplateExprId(self.template.expressions.len() as u32 - 1)
    }

    fn ty(&self, expr: TemplateExprId) -> Ty {
        self.template.expr(expr).ty
    }

    /// Emit a statement after the pending effect.
    fn stmt(&mut self, out: &mut Vec<TemplateStmtId>, kind: TemplateStmtKind) {
        self.flush_pending(out);
        self.absorbed = false;
        self.push_stmt(out, kind);
    }

    fn push_stmt(&mut self, out: &mut Vec<TemplateStmtId>, kind: TemplateStmtKind) {
        self.template.statements.push(TemplateStmt {
            kind,
            line: self.line,
        });
        out.push(TemplateStmtId(self.template.statements.len() as u32 - 1));
    }

    fn define(&mut self, value: ValueId, slot: Slot) {
        let carries_effect =
            std::mem::take(&mut self.absorbed) && matches!(slot, Slot::Expr { .. });
        self.slots[value.0 as usize] = Some(slot);
        self.defined.push(value);
        if carries_effect {
            self.pending = Some(value);
        }
    }

    /// Evaluate the pending value's expression now: into a temporary when something still reads
    /// it, for its effect otherwise.
    fn flush_pending(&mut self, out: &mut Vec<TemplateStmtId>) {
        if let Some(value) = self.pending.take() {
            self.materialize(out, value);
        }
    }

    /// Evaluate `value`'s expression here, leaving the value a read of the temporary holding it.
    fn materialize(&mut self, out: &mut Vec<TemplateStmtId>, value: ValueId) {
        let Some(Slot::Expr { expr, .. }) = self.slots[value.0 as usize].clone() else {
            return;
        };
        if self.uses[value.0 as usize] == 0 {
            self.push_stmt(out, TemplateStmtKind::Effect(expr));
            return;
        }
        let ty = self.ty(expr);
        let temporary = self.new_local(ty);
        self.push_stmt(
            out,
            TemplateStmtKind::Set {
                local: temporary,
                value: expr,
            },
        );
        let read = self.expr(TemplateExprKind::Local(temporary), ty);
        self.slots[value.0 as usize] = Some(Slot::Expr {
            expr: read,
            reads: vec![temporary],
        });
    }

    fn is_leaf(&self, expr: TemplateExprId) -> bool {
        matches!(
            self.template.expr(expr).kind,
            TemplateExprKind::Parameter(_)
                | TemplateExprKind::Local(_)
                | TemplateExprKind::Int(_)
                | TemplateExprKind::Long(_)
                | TemplateExprKind::Float(_)
                | TemplateExprKind::Double(_)
                | TemplateExprKind::String(_)
                | TemplateExprKind::Null
        )
    }

    /// Define `value` as `expr`, saving it to a temporary when it is read more than once and is
    /// not a leaf, so that the template stays a tree.
    fn define_pure(
        &mut self,
        out: &mut Vec<TemplateStmtId>,
        value: ValueId,
        expr: TemplateExprId,
        reads: Vec<TemplateLocalId>,
    ) {
        if self.uses[value.0 as usize] > 1 && !self.is_leaf(expr) {
            self.define_effect(out, Some(value), expr);
        } else {
            self.define(value, Slot::Expr { expr, reads });
        }
    }

    /// Evaluate `expr` here, after the pending effect. A value read exactly once waits to be
    /// built into its reader; one read more often is saved to a temporary; an unread one is
    /// evaluated for its effect alone.
    fn define_effect(
        &mut self,
        out: &mut Vec<TemplateStmtId>,
        value: Option<ValueId>,
        expr: TemplateExprId,
    ) {
        self.flush_pending(out);
        self.absorbed = false;
        match value {
            Some(value) if self.uses[value.0 as usize] == 1 => {
                self.define(
                    value,
                    Slot::Expr {
                        expr,
                        reads: vec![],
                    },
                );
                self.pending = Some(value);
            }
            Some(value) if self.uses[value.0 as usize] > 1 => {
                self.define(
                    value,
                    Slot::Expr {
                        expr,
                        reads: vec![],
                    },
                );
                self.materialize(out, value);
            }
            _ => self.push_stmt(out, TemplateStmtKind::Effect(expr)),
        }
    }

    fn slot(&self, value: ValueId) -> Result<&Slot, TemplateDecline> {
        self.slots[value.0 as usize]
            .as_ref()
            .ok_or_else(|| unsupported("a value read before it is defined"))
    }

    /// The expression `value` stands for at this read. A leaf is copied per read; any other
    /// expression is read once.
    fn consume(&mut self, value: ValueId) -> Result<TemplateExprId, TemplateDecline> {
        let Slot::Expr { expr, .. } = *self.slot(value)? else {
            return Err(unsupported(
                "an uninitialized instance or comparison used as a value",
            ));
        };
        self.uses[value.0 as usize] = self.uses[value.0 as usize].saturating_sub(1);
        if self.pending == Some(value) {
            self.pending = None;
            self.absorbed = true;
        }
        if self.is_leaf(expr) {
            let copy = self.template.expr(expr).clone();
            Ok(self.expr(copy.kind, copy.ty))
        } else {
            Ok(expr)
        }
    }

    /// `expr` as a value of `expected`: an `Int` constant stands for any int-like primitive, which
    /// the JVM pushes with the same instructions.
    fn coerce(&mut self, expr: TemplateExprId, expected: Ty) -> TemplateExprId {
        let current = self.template.expr(expr);
        if current.ty == Ty::Int && expected != Ty::Int && is_int_like(expected) {
            if let TemplateExprKind::Int(value) = current.kind {
                return self.expr(TemplateExprKind::Int(value), expected);
            }
        }
        expr
    }

    /// Before writing `local`, evaluate the pending effect and save every pending read of it.
    fn flush(&mut self, out: &mut Vec<TemplateStmtId>, local: TemplateLocalId) {
        self.flush_pending(out);
        let pending: Vec<ValueId> = self
            .defined
            .iter()
            .copied()
            .filter(|value| {
                self.uses[value.0 as usize] > 0
                    && matches!(&self.slots[value.0 as usize],
                        Some(Slot::Expr { reads, .. }) if reads.contains(&local))
            })
            .collect();
        for value in pending {
            self.materialize(out, value);
        }
    }

    fn shapes(&mut self, shapes: &[Shape]) -> Result<Vec<TemplateStmtId>, TemplateDecline> {
        let mut out = Vec::new();
        for shape in shapes {
            match shape {
                Shape::Loop { header, body } => {
                    let body = self.shapes(body)?;
                    self.stmt(
                        &mut out,
                        TemplateStmtKind::Loop {
                            label: TemplateLabel(header.0),
                            body,
                        },
                    );
                }
                Shape::Labeled { follow, body } => {
                    let body = self.shapes(body)?;
                    self.stmt(
                        &mut out,
                        TemplateStmtKind::Block {
                            label: TemplateLabel(follow.0),
                            body,
                        },
                    );
                }
                Shape::Node { block, exit } => self.node(&mut out, *block, exit)?,
            }
        }
        Ok(out)
    }

    fn node(
        &mut self,
        out: &mut Vec<TemplateStmtId>,
        block: BlockId,
        exit: &Exit,
    ) -> Result<(), TemplateDecline> {
        let body: &'a DecodedBody = self.body;
        let decoded = body.block(block);
        self.defined.clear();
        for (index, &value) in decoded.parameters.iter().enumerate() {
            let local = *self
                .block_parameters
                .get(&(block, index))
                .ok_or_else(|| unsupported("a block parameter no edge assigns first"))?;
            let ty = self.read_type(local)?;
            let expr = self.expr(TemplateExprKind::Local(local), ty);
            self.define(
                value,
                Slot::Expr {
                    expr,
                    reads: vec![local],
                },
            );
        }
        for op in &decoded.ops {
            self.line = op.line;
            self.op(out, &op.kind, op.result)?;
        }
        self.line = decoded.terminator_line;
        match (exit, &decoded.terminator) {
            (Exit::Return, Terminator::Return(value)) => {
                let value = match value {
                    Some(value) => {
                        let expr = self.consume(*value)?;
                        Some(self.coerce(expr, self.callable.ret))
                    }
                    None => None,
                };
                self.stmt(out, TemplateStmtKind::Return(value));
            }
            (Exit::Throw, Terminator::Throw(value)) => {
                let value = self.consume(*value)?;
                self.stmt(out, TemplateStmtKind::Throw(value));
            }
            (Exit::Goto(transfer), Terminator::Goto(edge)) => {
                self.transfer(out, edge, transfer)?;
            }
            (
                Exit::Branch { taken, not_taken },
                Terminator::Branch {
                    condition,
                    taken: taken_edge,
                    not_taken: not_taken_edge,
                },
            ) => {
                let reads_pending = self.pending.is_some_and(|pending| match *condition {
                    Condition::IntZero { value, .. } | Condition::Null { value, .. } => {
                        value == pending
                    }
                    Condition::Ints { lhs, rhs, .. } | Condition::References { lhs, rhs, .. } => {
                        lhs == pending || rhs == pending
                    }
                });
                if !reads_pending {
                    self.flush_pending(out);
                }
                let condition = self.condition(*condition)?;
                let line = self.line;
                let mut then = Vec::new();
                self.transfer(&mut then, taken_edge, taken)?;
                self.line = line;
                let mut otherwise = Vec::new();
                self.transfer(&mut otherwise, not_taken_edge, not_taken)?;
                self.line = line;
                self.stmt(
                    out,
                    TemplateStmtKind::If {
                        condition,
                        then,
                        otherwise,
                    },
                );
            }
            _ => unreachable!("a shape's exit follows its block's terminator"),
        }
        Ok(())
    }

    fn transfer(
        &mut self,
        out: &mut Vec<TemplateStmtId>,
        edge: &Edge,
        transfer: &Transfer,
    ) -> Result<(), TemplateDecline> {
        let body: &'a DecodedBody = self.body;
        let target = body.block(edge.target);
        let mut assignments = Vec::new();
        for (index, (&argument, &parameter)) in
            edge.arguments.iter().zip(&target.parameters).enumerate()
        {
            if argument == parameter {
                continue;
            }
            let expr = self.consume(argument)?;
            assignments.push((index, expr));
        }
        // An argument that reads one of the target's parameter locals goes through a temporary,
        // so the assignments act in parallel.
        let parameter_locals: Vec<TemplateLocalId> = (0..target.parameters.len())
            .filter_map(|index| self.block_parameters.get(&(edge.target, index)).copied())
            .collect();
        let reads_parameter = |emitter: &Self, expr: TemplateExprId| {
            matches!(emitter.template.expr(expr).kind,
                TemplateExprKind::Local(local) if parameter_locals.contains(&local))
        };
        let staged = assignments.len() > 1
            && assignments
                .iter()
                .any(|&(_, expr)| reads_parameter(self, expr) || !self.is_leaf(expr));
        let mut ready = Vec::with_capacity(assignments.len());
        for (index, expr) in assignments {
            if staged {
                let ty = self.ty(expr);
                let temporary = self.new_local(ty);
                self.stmt(
                    out,
                    TemplateStmtKind::Set {
                        local: temporary,
                        value: expr,
                    },
                );
                ready.push((index, self.expr(TemplateExprKind::Local(temporary), ty)));
            } else {
                ready.push((index, expr));
            }
        }
        for (index, expr) in ready {
            let local = self.block_parameter(edge.target, index, expr)?;
            let value = self.coerce(expr, self.template.local(local).ty);
            self.stmt(out, TemplateStmtKind::Set { local, value });
        }
        match transfer {
            Transfer::Continue(header) => {
                self.stmt(out, TemplateStmtKind::Continue(TemplateLabel(header.0)))
            }
            Transfer::Break(follow) => {
                self.stmt(out, TemplateStmtKind::Break(TemplateLabel(follow.0)))
            }
            Transfer::Inline(shapes) => {
                let code = self.shapes(shapes)?;
                out.extend(code);
            }
        }
        Ok(())
    }

    /// The local of `target`'s parameter `index`, typed by the first value assigned to it.
    fn block_parameter(
        &mut self,
        target: BlockId,
        index: usize,
        expr: TemplateExprId,
    ) -> Result<TemplateLocalId, TemplateDecline> {
        let ty = self.ty(expr);
        if let Some(&local) = self.block_parameters.get(&(target, index)) {
            self.merge_type(local, ty)
                .map_err(|()| unsupported("a block parameter of two types"))?;
            return Ok(local);
        }
        let local = self.new_local(ty);
        self.block_parameters.insert((target, index), local);
        Ok(local)
    }

    /// Admit a value of type `ty` into `local`. `null` makes a reference local nullable, and a
    /// local that only `null` reached so far takes the first reference type as nullable; an `Int`
    /// constant stands for any int-like primitive.
    fn merge_type(&mut self, local: TemplateLocalId, ty: Ty) -> Result<(), ()> {
        let declared = self.template.local(local).ty;
        let merged = if declared == ty
            || declared == Ty::nullable(ty)
            || (ty == Ty::Int && is_int_like(declared))
        {
            declared
        } else if ty == Ty::Null && primitive_box(declared).is_none() {
            Ty::nullable(declared)
        } else if declared == Ty::Null && primitive_box(ty).is_none() {
            Ty::nullable(ty)
        } else {
            return Err(());
        };
        self.template.locals[local.0 as usize].ty = merged;
        Ok(())
    }

    /// The type of `local` at a read: one that only `null` reached is not a type templates read.
    fn read_type(&self, local: TemplateLocalId) -> Result<Ty, TemplateDecline> {
        match self.template.local(local).ty {
            Ty::Null => Err(unsupported("a local typed only by null")),
            ty => Ok(ty),
        }
    }

    fn condition(&mut self, condition: Condition) -> Result<TemplateCondition, TemplateDecline> {
        Ok(match condition {
            Condition::IntZero {
                comparison: zero,
                value,
            } => {
                if let Slot::Comparison { op, lhs, rhs } = *self.slot(value)? {
                    self.uses[value.0 as usize] -= 1;
                    return self.folded_comparison(op, comparison(zero), lhs, rhs);
                }
                let expr = self.consume(value)?;
                if self.ty(expr) == Ty::Boolean {
                    let negated = match zero {
                        Comparison::Eq => true,
                        Comparison::Ne => false,
                        _ => return Err(unsupported("an ordering of a Boolean")),
                    };
                    TemplateCondition::Truth {
                        value: expr,
                        negated,
                    }
                } else {
                    let ty = self.ty(expr);
                    let zero_value = self.expr(TemplateExprKind::Int(0), ty);
                    TemplateCondition::Compare {
                        comparison: comparison(zero),
                        lhs: expr,
                        rhs: zero_value,
                        negated: false,
                    }
                }
            }
            Condition::Ints {
                comparison: ints,
                lhs,
                rhs,
            } => TemplateCondition::Compare {
                comparison: comparison(ints),
                lhs: self.consume(lhs)?,
                rhs: self.consume(rhs)?,
                negated: false,
            },
            Condition::References { equal, lhs, rhs } => TemplateCondition::Identical {
                lhs: self.consume(lhs)?,
                rhs: self.consume(rhs)?,
                negated: !equal,
            },
            Condition::Null { is_null, value } => TemplateCondition::IsNull {
                value: self.consume(value)?,
                negated: !is_null,
            },
        })
    }

    /// `cmp(lhs, rhs) <comparison> 0` as a direct comparison. A floating-point comparison whose
    /// NaN result takes the branch where IEEE 754 would not becomes the negated complement.
    fn folded_comparison(
        &mut self,
        op: BinaryOp,
        comparison: TemplateComparison,
        lhs: ValueId,
        rhs: ValueId,
    ) -> Result<TemplateCondition, TemplateDecline> {
        let nan_taken = match op {
            BinaryOp::Compare => None,
            BinaryOp::CompareNanGreater => Some(matches!(
                comparison,
                TemplateComparison::Gt | TemplateComparison::Ge | TemplateComparison::Ne
            )),
            BinaryOp::CompareNanLess => Some(matches!(
                comparison,
                TemplateComparison::Lt | TemplateComparison::Le | TemplateComparison::Ne
            )),
            _ => return Err(unsupported("a branch on an arithmetic value")),
        };
        // IEEE 754 makes only `!=` true when an operand is NaN.
        let ieee_taken = comparison == TemplateComparison::Ne;
        let lhs = self.consume(lhs)?;
        let rhs = self.consume(rhs)?;
        Ok(match nan_taken {
            Some(taken) if taken != ieee_taken => TemplateCondition::Compare {
                comparison: complement(comparison),
                lhs,
                rhs,
                negated: true,
            },
            _ => TemplateCondition::Compare {
                comparison,
                lhs,
                rhs,
                negated: false,
            },
        })
    }

    fn local_for_write(
        &mut self,
        local: LocalId,
        ty: Ty,
    ) -> Result<TemplateLocalId, TemplateDecline> {
        if self.parameter_of[local.0 as usize].is_some() {
            return Err(unsupported("a write to a parameter"));
        }
        if let Some(existing) = self.local_of[local.0 as usize] {
            self.merge_type(existing, ty)
                .map_err(|()| unsupported("a local slot reused for another type"))?;
            return Ok(existing);
        }
        let created = self.new_local(ty);
        self.local_of[local.0 as usize] = Some(created);
        Ok(created)
    }

    fn op(
        &mut self,
        out: &mut Vec<TemplateStmtId>,
        kind: &OpKind,
        result: Option<ValueId>,
    ) -> Result<(), TemplateDecline> {
        let produced = || result.expect("a value-producing operation has a result");
        match kind {
            OpKind::Null => {
                let expr = self.expr(TemplateExprKind::Null, Ty::Null);
                self.define(
                    produced(),
                    Slot::Expr {
                        expr,
                        reads: vec![],
                    },
                );
            }
            OpKind::Constant(constant) => {
                let (kind, ty) = match constant {
                    Constant::Int(value) => (TemplateExprKind::Int(*value), Ty::Int),
                    Constant::Long(value) => (TemplateExprKind::Long(*value), Ty::Long),
                    Constant::Float(bits) => (TemplateExprKind::Float(*bits), Ty::Float),
                    Constant::Double(bits) => (TemplateExprKind::Double(*bits), Ty::Double),
                    Constant::String(text) => (TemplateExprKind::String(text.clone()), Ty::String),
                    Constant::Class(_) | Constant::MethodType(_) | Constant::Handle(_) => {
                        return Err(unsupported("a class, method type or handle constant"))
                    }
                };
                let expr = self.expr(kind, ty);
                self.define(
                    produced(),
                    Slot::Expr {
                        expr,
                        reads: vec![],
                    },
                );
            }
            OpKind::LoadLocal(local) => {
                let (expr, reads) = match self.parameter_of[local.0 as usize] {
                    Some(ordinal) => {
                        let ty = self.callable.params[ordinal];
                        (self.expr(TemplateExprKind::Parameter(ordinal), ty), vec![])
                    }
                    None => {
                        let template_local = self.local_of[local.0 as usize]
                            .ok_or_else(|| unsupported("a local read before any write"))?;
                        let ty = self.read_type(template_local)?;
                        (
                            self.expr(TemplateExprKind::Local(template_local), ty),
                            vec![template_local],
                        )
                    }
                };
                self.define(produced(), Slot::Expr { expr, reads });
            }
            OpKind::StoreLocal { local, value } => {
                if !self.read[local.0 as usize] {
                    // A store nothing reads: kotlinc's `$i$f`/`$i$a` frame markers, or a dead
                    // local. Its value was already evaluated where it was produced.
                    self.consume(*value)?;
                    if self.parameter_of[local.0 as usize].is_some() {
                        return Err(unsupported("a write to a parameter"));
                    }
                    return Ok(());
                }
                let expr = self.consume(*value)?;
                let template_local = self.local_for_write(*local, self.ty(expr))?;
                self.flush(out, template_local);
                let expr = self.coerce(expr, self.template.local(template_local).ty);
                self.stmt(
                    out,
                    TemplateStmtKind::Set {
                        local: template_local,
                        value: expr,
                    },
                );
            }
            OpKind::IncrementLocal { local, delta } => {
                let template_local = self.local_for_write(*local, Ty::Int)?;
                let ty = self.template.local(template_local).ty;
                if ty != Ty::Int {
                    return Err(unsupported("an increment of a non-Int local"));
                }
                self.flush(out, template_local);
                let read = self.expr(TemplateExprKind::Local(template_local), ty);
                let step = self.expr(TemplateExprKind::Int(i32::from(*delta)), ty);
                let sum = self.expr(
                    TemplateExprKind::Binary {
                        op: TemplateBinaryOp::Add,
                        lhs: read,
                        rhs: step,
                    },
                    ty,
                );
                self.stmt(
                    out,
                    TemplateStmtKind::Set {
                        local: template_local,
                        value: sum,
                    },
                );
            }
            OpKind::Binary {
                op,
                category,
                lhs,
                rhs,
            } => self.binary(out, *op, *category, *lhs, *rhs, produced())?,
            OpKind::Negate { category, operand } => {
                let (operand_expr, reads) = self.consume_with_reads(*operand)?;
                let expr = self.expr(
                    TemplateExprKind::Negate(operand_expr),
                    category_type(*category),
                );
                self.define_pure(out, produced(), expr, reads);
            }
            OpKind::Convert { to, operand, .. } => {
                let (operand_expr, reads) = self.consume_with_reads(*operand)?;
                let ty = primitive_type(to).ok_or_else(|| unsupported("a reference conversion"))?;
                let expr = self.expr(TemplateExprKind::Convert(operand_expr), ty);
                self.define_pure(out, produced(), expr, reads);
            }
            OpKind::Invoke {
                kind,
                target,
                arguments,
            } => {
                let form = self.libraries.template_call(*kind, target)?;
                self.invoke(out, *kind, form, arguments, result)?;
            }
            OpKind::New { .. } => self.define(produced(), Slot::New),
            OpKind::NewArray { element, length } => {
                let element = JvmType::of_descriptor(element)
                    .as_ref()
                    .and_then(primitive_type)
                    .ok_or_else(|| unsupported("an array of references"))?;
                let length = self.consume(*length)?;
                let expr = self.expr(TemplateExprKind::NewArray { length }, Ty::array(element));
                self.define_effect(out, Some(produced()), expr);
            }
            OpKind::ArrayLength(array) => {
                let array = self.consume(*array)?;
                let expr = self.expr(TemplateExprKind::ArrayLength(array), Ty::Int);
                self.define_effect(out, Some(produced()), expr);
            }
            OpKind::ArrayLoad { array, index, .. } => {
                let array = self.consume(*array)?;
                let element = self
                    .ty(array)
                    .array_elem()
                    .ok_or_else(|| unsupported("an element read of a non-array"))?;
                let index = self.consume(*index)?;
                let expr = self.expr(TemplateExprKind::ArrayGet { array, index }, element);
                self.define_effect(out, Some(produced()), expr);
            }
            OpKind::ArrayStore {
                array,
                index,
                value,
                ..
            } => {
                let array = self.consume(*array)?;
                let element = self
                    .ty(array)
                    .array_elem()
                    .ok_or_else(|| unsupported("an element write of a non-array"))?;
                let index = self.consume(*index)?;
                let value = self.consume(*value)?;
                let value = self.coerce(value, element);
                self.stmt(
                    out,
                    TemplateStmtKind::ArraySet {
                        array,
                        index,
                        value,
                    },
                );
            }
            OpKind::CheckCast { class, value } => {
                let (expr, reads) = self.consume_with_reads(*value)?;
                if !self.restores(self.ty(expr), class) {
                    return Err(unsupported("a cast templates do not express"));
                }
                self.define(produced(), Slot::Expr { expr, reads });
            }
            OpKind::GetField { .. } | OpKind::PutField { .. } => {
                return Err(unsupported("a field access"))
            }
            OpKind::InvokeDynamic { .. } => return Err(unsupported("an invokedynamic")),
            OpKind::MultiNewArray { .. } => return Err(unsupported("a multi-dimensional array")),
            OpKind::InstanceOf { .. } => return Err(unsupported("a type test")),
            OpKind::MonitorEnter(_) | OpKind::MonitorExit(_) => {
                return Err(unsupported("a monitor"))
            }
        }
        Ok(())
    }

    fn consume_with_reads(
        &mut self,
        value: ValueId,
    ) -> Result<(TemplateExprId, Vec<TemplateLocalId>), TemplateDecline> {
        let reads = match self.slot(value)? {
            Slot::Expr { reads, .. } => reads.clone(),
            _ => Vec::new(),
        };
        Ok((self.consume(value)?, reads))
    }

    /// Whether `checkcast class` of a value of semantic type `ty` only restores what erasure took
    /// from it: the box of a primitive, or the class of a non-generic class type.
    fn restores(&self, ty: Ty, class: &str) -> bool {
        if primitive_box(ty) == Some(class) {
            return true;
        }
        let ty = match ty.kind() {
            TyKind::Nullable(inner) => inner,
            _ => ty,
        };
        matches!(ty.kind(), TyKind::Obj(name, arguments)
            if arguments.is_empty() && name == type_name(class))
    }

    fn binary(
        &mut self,
        out: &mut Vec<TemplateStmtId>,
        op: BinaryOp,
        category: Category,
        lhs: ValueId,
        rhs: ValueId,
        result: ValueId,
    ) -> Result<(), TemplateDecline> {
        let template_op = match op {
            BinaryOp::Add => TemplateBinaryOp::Add,
            BinaryOp::Sub => TemplateBinaryOp::Sub,
            BinaryOp::Mul => TemplateBinaryOp::Mul,
            BinaryOp::Div => TemplateBinaryOp::Div,
            BinaryOp::Rem => TemplateBinaryOp::Rem,
            BinaryOp::Shl => TemplateBinaryOp::Shl,
            BinaryOp::Shr => TemplateBinaryOp::Shr,
            BinaryOp::UShr => TemplateBinaryOp::UShr,
            BinaryOp::And => TemplateBinaryOp::And,
            BinaryOp::Or => TemplateBinaryOp::Or,
            BinaryOp::Xor => TemplateBinaryOp::Xor,
            BinaryOp::Compare | BinaryOp::CompareNanLess | BinaryOp::CompareNanGreater => {
                self.define(result, Slot::Comparison { op, lhs, rhs });
                return Ok(());
            }
        };
        let (lhs, mut reads) = self.consume_with_reads(lhs)?;
        let (rhs, rhs_reads) = self.consume_with_reads(rhs)?;
        reads.extend(rhs_reads);
        let logical = matches!(
            template_op,
            TemplateBinaryOp::And | TemplateBinaryOp::Or | TemplateBinaryOp::Xor
        ) && self.ty(lhs) == Ty::Boolean
            && self.ty(rhs) == Ty::Boolean;
        let ty = if logical {
            Ty::Boolean
        } else {
            category_type(category)
        };
        let expr = self.expr(
            TemplateExprKind::Binary {
                op: template_op,
                lhs,
                rhs,
            },
            ty,
        );
        // Integer division and remainder throw on a zero divisor: evaluate them in place.
        let throws = matches!(template_op, TemplateBinaryOp::Div | TemplateBinaryOp::Rem)
            && matches!(category, Category::Int | Category::Long);
        if throws {
            self.define_effect(out, Some(result), expr);
        } else {
            self.define_pure(out, result, expr, reads);
        }
        Ok(())
    }

    fn invoke(
        &mut self,
        out: &mut Vec<TemplateStmtId>,
        kind: InvokeKind,
        form: CallForm,
        arguments: &[ValueId],
        result: Option<ValueId>,
    ) -> Result<(), TemplateDecline> {
        match form {
            CallForm::Marker => {
                for &argument in arguments {
                    self.consume(argument)?;
                }
            }
            CallForm::ParameterNullCheck => {
                let [value, name] = arguments else {
                    unreachable!("the null check's descriptor takes two arguments")
                };
                let value = self.consume(*value)?;
                let name = self.consume(*name)?;
                let (TemplateExprKind::Parameter(parameter), TemplateExprKind::String(name)) = (
                    &self.template.expr(value).kind,
                    &self.template.expr(name).kind,
                ) else {
                    return Err(unsupported(
                        "a null check of a value other than a parameter",
                    ));
                };
                let (parameter, name) = (*parameter, name.clone());
                self.stmt(
                    out,
                    TemplateStmtKind::ParameterNullCheck { parameter, name },
                );
            }
            CallForm::Box { primitive } | CallForm::Unbox { primitive } => {
                let [value] = arguments else {
                    unreachable!("a box or unbox takes one operand")
                };
                let (expr, reads) = self.consume_with_reads(*value)?;
                let expr = self.coerce(expr, primitive);
                if self.ty(expr) != primitive {
                    return Err(unsupported("a box or unbox of another type"));
                }
                self.define(
                    result.expect("a box or unbox has a result"),
                    Slot::Expr { expr, reads },
                );
            }
            CallForm::ValueClassBox { class } => {
                let [value] = arguments else {
                    unreachable!("box-impl takes one operand")
                };
                let (expr, reads) = self.consume_with_reads(*value)?;
                if self.ty(expr) != Ty::obj_name(class) {
                    return Err(unsupported("a value-class box of its underlying value"));
                }
                self.define(
                    result.expect("box-impl has a result"),
                    Slot::Expr { expr, reads },
                );
            }
            CallForm::ValueClassUnbox { class, underlying } => {
                let [value] = arguments else {
                    unreachable!("unbox-impl takes its receiver")
                };
                let (operand, reads) = self.consume_with_reads(*value)?;
                if self.ty(operand) != Ty::obj_name(class) {
                    return Err(unsupported("a value-class unbox of another type"));
                }
                let expr = self.expr(TemplateExprKind::ValueClassUnderlying(operand), underlying);
                self.define_pure(out, result.expect("unbox-impl has a result"), expr, reads);
            }
            CallForm::FunctionInvoke { arity } => {
                let (function, rest) = arguments
                    .split_first()
                    .expect("an invoke has its function receiver");
                let function = self.consume(*function)?;
                let TemplateExprKind::Parameter(parameter) = self.template.expr(function).kind
                else {
                    return Err(unsupported(
                        "an invoke of a function value other than a parameter",
                    ));
                };
                let ret = lambda_parameter(self.callable, parameter, arity)
                    .ok_or_else(|| unsupported("an invoke of a parameter of another shape"))?;
                let TyKind::Fun(signature) = self.callable.params[parameter].kind() else {
                    unreachable!("lambda_parameter accepted a function type")
                };
                let mut lambda_arguments = Vec::with_capacity(rest.len());
                for (&argument, &expected) in rest.iter().zip(&signature.params) {
                    let expr = self.consume(argument)?;
                    lambda_arguments.push(self.coerce(expr, expected));
                }
                let expr = self.expr(
                    TemplateExprKind::InvokeLambda {
                        parameter,
                        arguments: lambda_arguments,
                    },
                    ret,
                );
                self.define_effect(out, result, expr);
            }
            CallForm::Declared { callee, result: ty } => {
                let (receiver, rest) = match (&callee, kind) {
                    (TemplateCallee::Constructor(_), InvokeKind::Special) => {
                        let (receiver, rest) = arguments
                            .split_first()
                            .expect("a constructor call has its instance");
                        if !matches!(self.slot(*receiver)?, Slot::New) {
                            return Err(unsupported("a constructor call on an initialized value"));
                        }
                        self.uses[receiver.0 as usize] -= 1;
                        (Some(*receiver), rest)
                    }
                    _ => (None, arguments),
                };
                let params: &[Ty] = match &callee {
                    TemplateCallee::Function(function) => &function.params,
                    TemplateCallee::Member(member) | TemplateCallee::Constructor(member) => {
                        &member.params
                    }
                };
                let dispatch = usize::from(matches!(callee, TemplateCallee::Member(_)));
                let params = params.to_vec();
                let mut call_arguments = Vec::with_capacity(rest.len());
                for (index, &argument) in rest.iter().enumerate() {
                    let expr = self.consume(argument)?;
                    let expected = index.checked_sub(dispatch).and_then(|at| params.get(at));
                    call_arguments.push(match expected {
                        Some(&expected) => self.coerce(expr, expected),
                        None => expr,
                    });
                }
                let expr = self.expr(
                    TemplateExprKind::Call {
                        callee,
                        arguments: call_arguments,
                    },
                    ty.unwrap_or(Ty::Unit),
                );
                self.define_effect(out, receiver.or(result), expr);
            }
        }
        Ok(())
    }
}
