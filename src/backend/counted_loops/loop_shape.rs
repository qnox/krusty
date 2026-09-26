//! `ProgressionLoopHeader`: the loop's variables and control flow for a built header.

use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr};
use crate::types::Ty;

use super::header::{LoopVariables, ProgressionHeader};
use super::{constant_value, is_unsigned, CountedLoop, CounterLoopStyle, Operand, Realizer};

impl Realizer<'_> {
    /// Declare the induction variable, `last` and `step` in kotlinc's order, then build the loop
    /// shape the header calls for. The result replaces the checked loop node.
    pub(super) fn progression_loop(&mut self, lp: CountedLoop) -> IrExpr {
        let CountedLoop {
            variable,
            variable_name,
            counter: ty,
            source,
            body,
            label,
        } = lp;
        let header = self.progression_header(&source, ty);
        let first_constant = constant_value(self.ir, header.first.value);
        let can_overflow = header.can_overflow(self.ir);
        let java_like = self.style == CounterLoopStyle::JavaLike && !header.last_is_inclusive;
        // The guarded do-while steps the induction variable before the body, so the loop variable is
        // a copy of it. The other shapes step after the body and use the induction variable as the
        // loop variable, except over unsigned elements, whose loop variable kotlinc always copies.
        let steps_first = self.style == CounterLoopStyle::JavaLike && !can_overflow && !java_like;
        let separate_loop_variable = steps_first || is_unsigned(ty);
        let mut statements = header.prelude.clone();
        let first = self.element_representation(header.first.value, ty);
        let (induction, induction_declaration) = if separate_loop_variable {
            let slot = self.allocate_temporary();
            let declaration = self.add(IrExpr::Variable {
                index: slot,
                ty,
                init: Some(first),
                named: false,
            });
            (slot, declaration)
        } else {
            let declaration =
                self.loop_variable_declaration(variable, variable_name.as_deref(), ty, first);
            (variable, declaration)
        };
        // The loop reads an unsigned `last` through a representation coercion, so kotlinc copies
        // anything but a constant.
        let last_operand = Operand {
            value: self.element_representation(header.last.value, ty),
            can_change: header.last.can_change
                || (is_unsigned(ty) && constant_value(self.ir, header.last.value).is_none()),
        };
        let mut last_statements = Vec::new();
        let (_, last) = self.loop_temporary(last_operand, ty, &mut last_statements);
        if header.is_reversed {
            statements.extend(last_statements);
            statements.push(induction_declaration);
        } else {
            statements.push(induction_declaration);
            statements.extend(last_statements);
        }
        let (_, step) = self.loop_temporary(header.step, header.step_ty, &mut statements);
        let variables = LoopVariables {
            induction,
            last,
            step,
        };
        let increment = self.increment_induction_variable(&variables, ty);
        // `val loopVariable = inductionVar` opening the body, when the two are apart.
        let (loop_variable, loop_variable_declaration) = if separate_loop_variable {
            let current = self.add(IrExpr::GetValue(induction));
            let declaration =
                self.loop_variable_declaration(variable, variable_name.as_deref(), ty, current);
            (variable, Some(declaration))
        } else {
            (induction, None)
        };
        let body = match loop_variable_declaration {
            Some(declaration) if !steps_first => self.add(IrExpr::Block {
                stmts: vec![declaration, body],
                value: None,
            }),
            _ => body,
        };
        let loop_expression = if can_overflow {
            // The induction variable can overflow past an inclusive bound, so the loop leaves by
            // comparing the loop variable with `last` before stepping, and the entry test guards
            // the whole loop.
            let current = self.add(IrExpr::GetValue(loop_variable));
            let at_last = self.add(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: current,
                rhs: last,
            });
            let leave = self.add(IrExpr::Break {
                label: Some(label.clone()),
            });
            let guard = self.add(IrExpr::When {
                branches: vec![(Some(at_last), leave)],
            });
            let update = self.add(IrExpr::Block {
                stmts: vec![guard, increment],
                value: None,
            });
            if self.style == CounterLoopStyle::PreTested {
                let condition = header.condition(self, &variables);
                self.add(IrExpr::While {
                    cond: condition,
                    body,
                    update: Some(update),
                    post_test: false,
                    label: Some(label),
                })
            } else {
                let always = self.add(IrExpr::Const(IrConst::Boolean(true)));
                let repeat = self.add(IrExpr::While {
                    cond: always,
                    body,
                    update: Some(update),
                    post_test: true,
                    label: Some(label),
                });
                self.guarded_unless_entered(&header, &variables, first_constant, repeat)
            }
        } else if java_like || self.style == CounterLoopStyle::PreTested {
            let condition = header.condition(self, &variables);
            self.add(IrExpr::While {
                cond: condition,
                body,
                update: Some(increment),
                post_test: false,
                label: Some(label),
            })
        } else {
            // `val loopVariable = inductionVar; inductionVar += step; body` while the bound holds.
            let mut stmts: Vec<ExprId> = loop_variable_declaration.into_iter().collect();
            stmts.extend([increment, body]);
            let body = self.add(IrExpr::Block { stmts, value: None });
            let condition = header.condition(self, &variables);
            let repeat = self.add(IrExpr::While {
                cond: condition,
                body,
                update: None,
                post_test: true,
                label: Some(label),
            });
            self.guarded_unless_entered(&header, &variables, first_constant, repeat)
        };
        statements.push(loop_expression);
        IrExpr::Block {
            stmts: statements,
            value: None,
        }
    }

    /// `if (<entry condition>) <loop>`. kotlinc folds an `Int`-sized comparison between two
    /// constants before emission, so a guard that always holds disappears; a `Long` comparison
    /// (`lcmp`) is not folded.
    fn guarded_unless_entered(
        &mut self,
        header: &ProgressionHeader,
        variables: &LoopVariables,
        first_constant: Option<i64>,
        repeat: ExprId,
    ) -> ExprId {
        let last_constant = constant_value(self.ir, variables.last)
            .filter(|_| !matches!(header.ty, Ty::Long | Ty::UInt | Ty::ULong));
        let entered = first_constant
            .zip(last_constant)
            .and_then(|(first, last)| header.holds_between(first, last));
        if entered == Some(true) {
            return repeat;
        }
        let entry = header.condition(self, variables);
        self.add(IrExpr::When {
            branches: vec![(Some(entry), repeat)],
        })
    }

    /// `inductionVar += step`; a `Char` induction variable is narrowed back after the `Int` add.
    fn increment_induction_variable(&mut self, variables: &LoopVariables, ty: Ty) -> ExprId {
        let current = self.add(IrExpr::GetValue(variables.induction));
        let stepped = self.add(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: current,
            rhs: variables.step,
        });
        let stepped = if ty == Ty::Char {
            self.range_bound(stepped, ty)
        } else {
            stepped
        };
        self.add(IrExpr::SetValue {
            var: variables.induction,
            value: stepped,
        })
    }
}
