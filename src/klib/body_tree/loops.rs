//! Loops: `while` and `do`-`while`, the jumps that leave or resume them, and the counted loops
//! common IR lowers a `for` over a range to.

use super::{
    call, member_signature, BodyExprId, BodyKind, BodyLoop, BodyStatement, BodyVariable, JumpKind,
    Translation, ValueRef,
};
use crate::fir::FirRangeOperation;
use crate::ir::{ExprId, IrBinOp, IrConst, IrProgressionSource};
use crate::klib_lowering::ir_builtins::BuiltinOperator;
use crate::types::Ty;

impl Translation<'_, '_> {
    /// A `while` or `do`-`while` loop.
    ///
    /// A loop with an `update` is a lowered `for`, whose `continue` runs the update before the
    /// next test. Kotlin IR has no such loop, so the body is written inside a `do { } while
    /// (false)` the loop's `continue` breaks out of, followed by the update.
    pub(super) fn while_loop(
        &mut self,
        condition: ExprId,
        body: ExprId,
        update: Option<ExprId>,
        post_test: bool,
        label: Option<&str>,
    ) -> Option<BodyExprId> {
        if update.is_some() && post_test {
            return None;
        }
        let id = self.fresh_loop();
        let continue_target = match update {
            Some(_) => LoopTarget::Break(self.fresh_loop()),
            None => LoopTarget::Continue(id),
        };
        self.frame.loops.push(EnclosingLoop {
            label: label.map(str::to_owned),
            break_id: id,
            continue_target,
        });
        // A `do`-`while` condition sees the body's variables, so the body is translated first in
        // both forms; the operands keep their own order on the wire.
        let translated_body = self.expr(body);
        self.frame.loops.pop();
        let mut body = translated_body?;
        if let (Some(update), LoopTarget::Break(inner)) = (update, continue_target) {
            let update = self.expr(update)?;
            let never = self
                .body
                .push(Ty::Boolean, BodyKind::Const(IrConst::Boolean(false)));
            let iteration = self.push(
                Ty::Unit,
                BodyKind::Loop(Box::new(BodyLoop {
                    id: inner,
                    post_test: true,
                    condition: never,
                    body,
                    origin: "DO_WHILE_LOOP",
                })),
            )?;
            body = self.push(
                Ty::Unit,
                BodyKind::Block {
                    statements: vec![
                        BodyStatement::Expression(iteration),
                        BodyStatement::Expression(update),
                    ],
                    origin: None,
                },
            )?;
        }
        let condition = self.expr(condition)?;
        if self.ty(condition) != Ty::Boolean {
            return None;
        }
        self.push(
            Ty::Unit,
            BodyKind::Loop(Box::new(BodyLoop {
                id,
                post_test,
                condition,
                body,
                origin: if post_test {
                    "DO_WHILE_LOOP"
                } else {
                    "WHILE_LOOP"
                },
            })),
        )
    }

    pub(super) fn fresh_loop(&mut self) -> u32 {
        let id = self.next_loop;
        self.next_loop += 1;
        id
    }

    /// A counted `for` over a range literal or an `indices` range of an `Int` or `Long` counter,
    /// as kotlinc's `ForLoopsLowering` counts it. kotlinc serializes the iterator form that lowering
    /// starts from, and a consumer lowers only blocks carrying its `FOR_LOOP` origins, so the
    /// counted form is written as plain blocks and loops: the bounds are read once, the loop variable is
    /// declared at the top of each iteration, and the counter is stepped before the body runs.
    /// An inclusive bound is tested after the body against the iteration's own value, so the
    /// counter never steps past the type's last value.
    pub(super) fn range_loop(&mut self, found: RangeLoop<'_>) -> Option<BodyExprId> {
        let counter = found.counter;
        if counter != Ty::Int && counter != Ty::Long {
            return None;
        }
        let (start, end, operation) = match found.source {
            IrProgressionSource::Literal {
                operation,
                start,
                end,
            } => (self.expr(*start)?, self.expr(*end)?, *operation),
            IrProgressionSource::Indices { size } => {
                let zero = self.body.push(Ty::Int, BodyKind::Const(IrConst::Int(0)));
                (zero, self.expr(*size)?, FirRangeOperation::Until)
            }
            _ => return None,
        };
        if self.ty(start) != counter || self.ty(end) != counter {
            return None;
        }
        let mut statements = Vec::new();
        let last = self.temporary(&mut statements, counter, end, false);
        let induction = self.temporary(&mut statements, counter, start, true);
        let id = self.fresh_loop();
        self.frame.loops.push(EnclosingLoop {
            label: Some(found.label.to_owned()),
            break_id: id,
            continue_target: LoopTarget::Continue(id),
        });
        let iteration = self.counted_iteration(&found, induction, operation);
        self.frame.loops.pop();
        let (body, variable) = iteration?;
        let read = |translation: &mut Self, local| {
            translation
                .body
                .push(counter, BodyKind::GetValue(ValueRef::Local(local)))
        };
        let (entry, post_test) = match operation {
            FirRangeOperation::Until | FirRangeOperation::OpenEnd => (IrBinOp::Lt, false),
            FirRangeOperation::Through => (IrBinOp::Le, true),
            FirRangeOperation::DownTo => (IrBinOp::Ge, true),
        };
        let (current, bound) = (read(self, induction), read(self, last));
        let entered = self.relation(entry, current, bound)?;
        let condition = if post_test {
            let value = read(self, variable);
            let bound = read(self, last);
            self.not_equal(value, bound)?
        } else {
            entered
        };
        let counted = self.push(
            Ty::Unit,
            BodyKind::Loop(Box::new(BodyLoop {
                id,
                post_test,
                condition,
                body,
                origin: if post_test {
                    "DO_WHILE_LOOP"
                } else {
                    "WHILE_LOOP"
                },
            })),
        )?;
        let counted = if post_test {
            self.push(
                Ty::Unit,
                BodyKind::When {
                    branches: vec![(entered, counted)],
                    origin: Some("IF"),
                },
            )?
        } else {
            counted
        };
        statements.push(BodyStatement::Expression(counted));
        self.push(
            Ty::Unit,
            BodyKind::Block {
                statements,
                origin: None,
            },
        )
    }

    /// One iteration of a counted loop: the loop variable, the counter's step, then the body.
    /// Answers the iteration and the loop variable's id.
    pub(super) fn counted_iteration(
        &mut self,
        found: &RangeLoop<'_>,
        induction: u32,
        operation: FirRangeOperation,
    ) -> Option<(BodyExprId, u32)> {
        let counter = found.counter;
        let current = self
            .body
            .push(counter, BodyKind::GetValue(ValueRef::Local(induction)));
        let local = self.declare(found.variable, counter)?;
        let variable = BodyStatement::Variable(BodyVariable {
            local,
            name: Some(found.variable_name?.to_owned()),
            ty: counter,
            mutable: false,
            initializer: Some(current),
            origin: "DEFINED",
        });
        let step = self.body.push(
            counter,
            BodyKind::Const(if counter == Ty::Long {
                IrConst::Long(1)
            } else {
                IrConst::Int(1)
            }),
        );
        let name = match operation {
            FirRangeOperation::DownTo => "minus",
            _ => "plus",
        };
        let current = self
            .body
            .push(counter, BodyKind::GetValue(ValueRef::Local(induction)));
        let target = member_signature(counter, name, Some(counter))?;
        let stepped = self.push(counter, call(target, vec![current, step], None))?;
        let advance = self.push(
            Ty::Unit,
            BodyKind::SetValue {
                local: induction,
                value: stepped,
            },
        )?;
        let body = self.expr(found.body)?;
        let iteration = self.push(
            Ty::Unit,
            BodyKind::Block {
                statements: vec![
                    variable,
                    BodyStatement::Expression(advance),
                    BodyStatement::Expression(body),
                ],
                origin: None,
            },
        )?;
        Some((iteration, local))
    }

    /// Declares a compiler temporary holding `value` among `statements`.
    pub(super) fn temporary(
        &mut self,
        statements: &mut Vec<BodyStatement>,
        ty: Ty,
        value: BodyExprId,
        mutable: bool,
    ) -> u32 {
        let local = self.fresh_local();
        statements.push(BodyStatement::Variable(BodyVariable {
            local,
            name: None,
            ty,
            mutable,
            initializer: Some(value),
            origin: "IR_TEMPORARY_VARIABLE",
        }));
        local
    }

    /// `left != right`, kotlinc's negated `EQEQ`.
    pub(super) fn not_equal(&mut self, left: BodyExprId, right: BodyExprId) -> Option<BodyExprId> {
        let equals = self.builtin(BuiltinOperator::Equals)?;
        let equal = self.push(Ty::Boolean, call(equals, vec![left, right], Some("EXCLEQ")))?;
        let not = self.builtin(BuiltinOperator::Not)?;
        self.push(Ty::Boolean, call(not, vec![equal], Some("EXCLEQ")))
    }

    /// `break` or `continue` of the innermost loop, or of the enclosing loop `label` names.
    pub(super) fn jump(&mut self, kind: JumpKind, label: Option<&str>) -> Option<BodyExprId> {
        let target = match label {
            None => self.frame.loops.last()?,
            Some(label) => self
                .frame
                .loops
                .iter()
                .rev()
                .find(|found| found.label.as_deref() == Some(label))?,
        };
        let (kind, loop_id) = match (kind, target.continue_target) {
            (JumpKind::Break, _) => (JumpKind::Break, target.break_id),
            (JumpKind::Continue, LoopTarget::Continue(id)) => (JumpKind::Continue, id),
            (JumpKind::Continue, LoopTarget::Break(id)) => (JumpKind::Break, id),
        };
        self.push(Ty::Nothing, BodyKind::Jump { kind, loop_id })
    }
}

/// A common-IR loop a jump in its body may target, and the Kotlin IR loops that realize it.
pub(super) struct EnclosingLoop {
    label: Option<String>,
    break_id: u32,
    continue_target: LoopTarget,
}

/// What a `continue` of a loop jumps to.
#[derive(Clone, Copy)]
pub(super) enum LoopTarget {
    /// The loop's own next test.
    Continue(u32),
    /// The end of the loop's iteration block, which then runs the loop's update.
    Break(u32),
}

/// The checked pieces of a common-IR counted loop over a progression.
pub(super) struct RangeLoop<'a> {
    pub(super) variable: u32,
    pub(super) variable_name: Option<&'a str>,
    pub(super) counter: Ty,
    pub(super) source: &'a IrProgressionSource,
    pub(super) body: ExprId,
    pub(super) label: &'a str,
}
