//! The producer's Elvis temporary and its exact null proof, lowered as checked FIR Elvis.

use super::super::ir_builtins::{BuiltinCallDisposition, BuiltinOperator};
use super::{is_value_of, mismatch, BodyLowering, Lowered};
use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr};
use crate::metadata::klib_ir::tree::{
    KlibIrExprId, KlibIrExprKind, KlibIrStatement, KlibIrVariableId,
};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSignature, KlibIrSymbolKind};
use crate::types::Ty;

struct ElvisShape {
    temporary: KlibIrVariableId,
    input: Ty,
    fallback: KlibIrExprId,
}

impl BodyLowering<'_> {
    pub(super) fn elvis_block(
        &mut self,
        statements: &[KlibIrStatement],
        ty: Ty,
    ) -> Lowered<ExprId> {
        let shape = self.elvis_shape(statements, ty)?;
        // FIR always snapshots the left value, including a plain immutable parameter. The
        // general compiler-temporary alias optimization is not the Elvis operation.
        let variable = self.unnamed_local(shape.temporary)?;
        let symbol = &self.arena.variable(shape.temporary).base.symbol;
        let local = self
            .locals
            .get(symbol)
            .expect("the Elvis temporary was declared");
        let condition_read = self.elvis_read(local.index);
        let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
        let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: condition_read,
            rhs: null,
        });
        let fallback = self.expression(shape.fallback)?;
        let fallback_type = self.lowered_type(fallback);
        let fallback = self.elvis_result(fallback, fallback_type, ty)?;
        let present = self.elvis_read(local.index);
        // Only this read has the authenticated null proof. Its storage remains the temporary's
        // declared type; the branch conversion consumes its proven non-null value.
        let present = self.elvis_result(present, shape.input.non_null(), ty)?;
        let when = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(condition), fallback), (None, present)],
        });
        self.ir.whens.elvises.insert(when, 0); // Serialized bodies supply no source line.
        self.ir.logical_types.insert(when, ty);
        let block = self.ir.add_expr(IrExpr::Block {
            stmts: vec![variable],
            value: Some(when),
        });
        Ok(self.typed(block, ty))
    }

    fn elvis_shape(&self, statements: &[KlibIrStatement], ty: Ty) -> Lowered<ElvisShape> {
        let &[KlibIrStatement::Variable(temporary), KlibIrStatement::Expression(conditional)] =
            statements
        else {
            return Err(mismatch(
                "an Elvis block does not contain one temporary and a conditional",
            ));
        };
        let variable = self.arena.variable(temporary);
        if variable.base.origin != "IR_TEMPORARY_VARIABLE"
            || variable.base.symbol.kind != KlibIrSymbolKind::Variable
        {
            return Err(mismatch(
                "an Elvis block does not declare a compiler temporary",
            ));
        }
        let input = super::semantic_type(self.arena, self.header.type_parameters(), variable.ty)?;
        let expression = self.arena.expr(conditional);
        let KlibIrExprKind::When {
            branches,
            origin: None,
        } = &expression.kind
        else {
            return Err(mismatch(
                "an Elvis block does not end in its origin-less conditional",
            ));
        };
        let [absent, present] = branches.as_slice() else {
            return Err(mismatch("an Elvis conditional does not have two branches"));
        };
        if self.ty(expression, "Elvis conditional")? != ty
            || self.ty(self.arena.expr(present.condition), "Elvis else condition")? != Ty::Boolean
            || !matches!(
                self.arena.expr(present.condition).kind,
                KlibIrExprKind::Const(KlibIrConstant::Boolean(true))
            )
        {
            return Err(mismatch(
                "an Elvis conditional has another type or else condition",
            ));
        }
        let (read, null) = self.elvis_null_test(absent.condition)?;
        let read = self.arena.expr(read);
        let null = self.arena.expr(null);
        let result = self.arena.expr(present.result);
        if !matches!(&read.kind, KlibIrExprKind::GetValue { symbol, .. } if symbol == &variable.base.symbol)
            || self.ty(read, "Elvis temporary read")? != input
            || !matches!(null.kind, KlibIrExprKind::Const(KlibIrConstant::Null))
            || self.ty(null, "Elvis null operand")? != Ty::nullable(Ty::Nothing)
            || !matches!(&result.kind, KlibIrExprKind::GetValue { symbol, .. } if symbol == &variable.base.symbol)
        {
            return Err(mismatch(
                "an Elvis null proof does not name its exact temporary",
            ));
        }
        let result_type = self.ty(result, "Elvis present read")?;
        if result_type != input && result_type != input.non_null() {
            return Err(mismatch(
                "an Elvis present read has another type than its temporary",
            ));
        }
        Ok(ElvisShape {
            temporary,
            input,
            fallback: absent.result,
        })
    }

    fn elvis_null_test(&self, condition: KlibIrExprId) -> Lowered<(KlibIrExprId, KlibIrExprId)> {
        let condition = self.arena.expr(condition);
        let KlibIrExprKind::Call {
            access,
            super_qualifier: None,
        } = &condition.kind
        else {
            return Err(mismatch(
                "an Elvis condition is not its compiler null comparison",
            ));
        };
        let KlibIrSignature::Public(signature) = &access.symbol.signature else {
            return Err(mismatch(
                "an Elvis condition is not its compiler null comparison",
            ));
        };
        if access.symbol.kind != KlibIrSymbolKind::Function {
            return Err(mismatch(
                "an Elvis condition is not its compiler null comparison",
            ));
        }
        // As for ordinary builtin calls, authenticate retained/ambiguous declaration obligations
        // before validating the operands of an operation eligible for common lowering.
        let disposition = self
            .builtins
            .call_disposition(signature, || self.linker.requires_declaration(signature))?;
        if disposition != BuiltinCallDisposition::Common(BuiltinOperator::Equals)
            || !access.type_arguments.is_empty()
            || self.ty(condition, "Elvis null comparison")? != Ty::Boolean
        {
            return Err(mismatch(
                "an Elvis condition is not its compiler null comparison",
            ));
        }
        let arguments = super::calls::arguments(access);
        let [Some(read), Some(null)] = arguments.as_slice() else {
            return Err(mismatch(
                "an Elvis null comparison does not have two operands",
            ));
        };
        Ok((*read, *null))
    }

    fn elvis_read(&mut self, index: u32) -> ExprId {
        // As in FIR, these are anonymous snapshot reads, not reads of a source binding.
        self.ir.add_expr(IrExpr::GetValue(index))
    }

    fn elvis_result(&mut self, value: ExprId, proven: Ty, target: Ty) -> Lowered<ExprId> {
        if !is_value_of(proven, target)
            && proven != Ty::Nothing
            && !self
                .linker
                .is_subtype(self.header.type_parameters(), proven, target)
        {
            return Err(super::KlibBodyDeclineReason::ImplicitConversion);
        }
        let coerced = if target == Ty::Unit {
            let unit = self.ir.add_expr(IrExpr::UnitInstance);
            self.ir.add_expr(IrExpr::Block {
                stmts: vec![value],
                value: Some(unit),
            })
        } else {
            self.coercion(value, target)
        };
        Ok(coerced)
    }
}
