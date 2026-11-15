//! The guards an `equals` implementation opens with before it reads its parameter as its own
//! classifier: a generated data-class `equals`, and a source `equals` checked under an
//! `@EqualityBound` contract.

use crate::ir::{ExprId, IrBinOp, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// `if (this === other) return true`, when `identity` asks for it, then
/// `if (other !is bound) return false`. A value class has no identity, so its `equals` keeps only
/// the type test.
pub(super) fn identity_and_type_guards(
    ir: &mut IrFile,
    this_slot: u32,
    other_slot: u32,
    bound: Ty,
    identity: bool,
) -> Vec<ExprId> {
    let mut guards = Vec::with_capacity(2);
    if identity {
        let this_value = ir.add_expr(IrExpr::GetValue(this_slot));
        let other_value = ir.add_expr(IrExpr::GetValue(other_slot));
        let identical = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::RefEq,
            lhs: this_value,
            rhs: other_value,
        });
        guards.push(return_guard(ir, identical, true));
    }
    let other_value = ir.add_expr(IrExpr::GetValue(other_slot));
    let wrong_type = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::NotInstanceOf,
        arg: other_value,
        type_operand: bound,
    });
    guards.push(return_guard(ir, wrong_type, false));
    guards
}

/// `if (condition) return value`.
pub(super) fn return_guard(ir: &mut IrFile, condition: ExprId, value: bool) -> ExprId {
    let value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(value)));
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    ir.set_logical_type(returned, Ty::Nothing);
    let branch = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    ir.set_logical_type(branch, Ty::Unit);
    // An `if` without an `else` is a `Unit` statement, as the checker types the source form.
    let guard = ir.add_expr(IrExpr::When {
        branches: vec![(Some(condition), branch)],
    });
    ir.set_logical_type(guard, Ty::Unit);
    guard
}

impl super::BodyLowering<'_> {
    /// The guards a source `equals` checked under an `@EqualityBound` contract opens with, ahead
    /// of its source statements: they are what lets the body read its parameter as the bound.
    pub(super) fn equality_bound_prologue(
        &mut self,
        declaration: crate::fir::DeclarationId,
    ) -> Result<Vec<ExprId>, super::FirLoweringFailure> {
        let Some(guard) = self.body.equality_bound_guard() else {
            return Ok(Vec::new());
        };
        let origin = self
            .body
            .parameters()
            .iter()
            .find(|parameter| parameter.value == guard.parameter)
            .map(|parameter| parameter.origin)
            .expect("an equality bound guards one of the body's own parameters");
        let this_slot = self
            .dispatch_receiver_slot()
            .ok_or(super::FirLoweringFailure::MissingImplicitReceiver { origin })?;
        let value_class = self
            .index
            .enclosing_classifier(declaration)
            .and_then(|classifier| self.index.declaration_header(classifier.declaration))
            .is_some_and(|header| {
                header
                    .flags
                    .has(crate::fir::DeclarationFlags::VALUE_KEYWORD)
            });
        let other_slot = self.value_slot(guard.parameter);
        Ok(identity_and_type_guards(
            self.ir,
            this_slot,
            other_slot,
            guard.bound.get(),
            !value_class,
        ))
    }
}
