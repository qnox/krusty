//! Which subject `when`s over an enum kotlinc's common `EnumWhenLowering` turns into a switch on
//! a table of keys.
//!
//! Common lowering builds a subject `when` as `Block { val subject = …; when { … } }`. Such a
//! `when` switches on keys when its subject is an enum compared with one of its entries and no
//! branch tests the subject against a computed value. The keys come from a table its emitting
//! class owns (the JVM's `$WhenMappings`), so code holding such a `when` reads state of that class.

use super::{ExprId, IrBinOp, IrExpr, IrFile};
use crate::types::{Ty, TypeName};

/// A subject `when` that switches on an enum's keys.
pub(crate) struct EnumSwitchWhen {
    /// The block holding the subject temporary and the `when`.
    pub(crate) block: ExprId,
    /// The subject temporary's value index.
    pub(crate) subject: u32,
    pub(crate) classifier: TypeName,
    pub(crate) nullable: bool,
    pub(crate) when: ExprId,
}

impl EnumSwitchWhen {
    /// The operand of a comparison `lhs == rhs` that is compared with the subject, when one side
    /// reads the subject.
    pub(crate) fn compared_operand(&self, ir: &IrFile, lhs: ExprId, rhs: ExprId) -> Option<ExprId> {
        let reads_subject = |operand: ExprId| matches!(ir.expr(operand), IrExpr::GetValue(value) if *value == self.subject);
        if reads_subject(lhs) {
            Some(rhs)
        } else if reads_subject(rhs) {
            Some(lhs)
        } else {
            None
        }
    }

    fn compares_an_entry(&self, ir: &IrFile, expression: ExprId) -> bool {
        match ir.expr(expression) {
            IrExpr::When { branches } => branches.iter().any(|(condition, result)| {
                condition.is_some_and(|condition| self.compares_an_entry(ir, condition))
                    || matches!(ir.expr(*result), IrExpr::When { .. })
                        && self.compares_an_entry(ir, *result)
            }),
            &IrExpr::Equality {
                op: IrBinOp::Eq,
                mode: super::EqualityMode::Structural,
                lhs,
                rhs,
            } => self.compared_operand(ir, lhs, rhs).is_some_and(|other| {
                matches!(
                    ir.expr(other),
                    IrExpr::EnumEntry { classifier, .. } if *classifier == self.classifier
                )
            }),
            _ => false,
        }
    }

    /// kotlinc's `possibleToGenerateJumpTable`: no branch tests the subject with another
    /// operation, or against a value that a call computes.
    fn possible_to_generate_jump_table(&self, ir: &IrFile) -> bool {
        let IrExpr::When { branches } = ir.expr(self.when) else {
            return false;
        };
        branches.iter().all(|(condition, _)| {
            let Some(condition) = condition else {
                return true;
            };
            match ir.expr(*condition) {
                &IrExpr::Equality {
                    op: IrBinOp::Eq,
                    mode: super::EqualityMode::Structural,
                    lhs,
                    rhs,
                } => self.compared_operand(ir, lhs, rhs).is_none_or(|other| {
                    matches!(
                        ir.expr(other),
                        IrExpr::EnumEntry { .. }
                            | IrExpr::Const(_)
                            | IrExpr::GetValue(_)
                            | IrExpr::GetStatic(_)
                            | IrExpr::GetField { .. }
                    )
                }),
                // An `||` chain of clauses, a guard, and a type test are not calls.
                IrExpr::When { .. } | IrExpr::TypeOp { .. } | IrExpr::GetValue(_) => true,
                _ => false,
            }
        })
    }
}

impl IrFile {
    /// `block` as a subject `when` that switches on its enum subject's keys, if it is one.
    pub(crate) fn enum_switch_when(&self, block: ExprId) -> Option<EnumSwitchWhen> {
        let IrExpr::Block {
            stmts,
            value: Some(when),
        } = self.expr(block)
        else {
            return None;
        };
        let [declaration] = stmts.as_slice() else {
            return None;
        };
        let IrExpr::Variable {
            index: subject, ty, ..
        } = self.expr(*declaration)
        else {
            return None;
        };
        if !matches!(self.expr(*when), IrExpr::When { .. }) {
            return None;
        }
        let (classifier, nullable) = match *ty {
            Ty::Obj(classifier, _) => (classifier, false),
            Ty::Nullable(Ty::Obj(classifier, _)) => (*classifier, true),
            _ => return None,
        };
        let candidate = EnumSwitchWhen {
            block,
            subject: *subject,
            classifier,
            nullable,
            when: *when,
        };
        (candidate.compares_an_entry(self, *when)
            && candidate.possible_to_generate_jump_table(self))
        .then_some(candidate)
    }
}
