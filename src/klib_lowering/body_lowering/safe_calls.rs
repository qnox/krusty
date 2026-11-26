//! The producer's safe-call temporary, its exact null proof and the guarded selector, lowered as
//! checked FIR lowers `receiver?.selector`.

use super::{mismatch, BodyLowering, Lowered};
use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr, IrTypeOp};
use crate::metadata::klib_ir::tree::{KlibIrExprId, KlibIrExprKind, KlibIrStatement};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSymbolKind};
use crate::types::Ty;

/// A read whose non-null proof is in scope where it is lowered.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum NullProvenRead {
    /// A safe-call temporary, inside the selector its `null` test guards.
    Value(crate::metadata::klib_ir::KlibIrSymbol),
    /// A field read on a value, in the `else` of a compiler-generated `when` testing the same read
    /// against `null`. A generated data or value class member reads the field it tested without
    /// the cast a source smart cast serializes.
    Field {
        field: crate::metadata::klib_ir::KlibIrSymbol,
        receiver: crate::metadata::klib_ir::KlibIrSymbol,
    },
}

impl NullProvenRead {
    /// The proof a `read` names, when it is a field read on a value.
    fn field_read(
        arena: &crate::metadata::klib_ir::tree::KlibIrArena,
        read: KlibIrExprId,
    ) -> Option<Self> {
        let KlibIrExprKind::GetField {
            symbol,
            super_qualifier: None,
            receiver: Some(receiver),
            ..
        } = &arena.expr(read).kind
        else {
            return None;
        };
        let KlibIrExprKind::GetValue {
            symbol: receiver, ..
        } = &arena.expr(*receiver).kind
        else {
            return None;
        };
        Some(Self::Field {
            field: symbol.clone(),
            receiver: receiver.clone(),
        })
    }
}

impl BodyLowering<'_> {
    /// `{ val tmp = receiver; when { tmp == null -> null; else -> selector } }`, whose selector
    /// reads `tmp` through its implicit non-null cast.
    pub(super) fn safe_call_block(
        &mut self,
        statements: &[KlibIrStatement],
        ty: Ty,
    ) -> Lowered<ExprId> {
        let &[KlibIrStatement::Variable(temporary), KlibIrStatement::Expression(conditional)] =
            statements
        else {
            return Err(mismatch(
                "a safe call does not contain one temporary and a conditional",
            ));
        };
        let variable = self.arena.variable(temporary);
        if variable.base.origin != "IR_TEMPORARY_VARIABLE"
            || variable.base.symbol.kind != KlibIrSymbolKind::Variable
        {
            return Err(mismatch(
                "a safe call does not declare a compiler temporary",
            ));
        }
        let symbol = variable.base.symbol.clone();
        let input = self.semantic(variable.ty)?;
        if !input.is_nullable() || !ty.is_nullable() {
            return Err(mismatch("a safe call is not over and of nullable types"));
        }
        let expression = self.arena.expr(conditional);
        let KlibIrExprKind::When {
            branches,
            origin: None,
        } = &expression.kind
        else {
            return Err(mismatch(
                "a safe call does not end in its origin-less conditional",
            ));
        };
        let [absent, present] = branches.as_slice() else {
            return Err(mismatch(
                "a safe-call conditional does not have two branches",
            ));
        };
        if self.ty(expression, "safe-call conditional")? != ty
            || !matches!(
                self.arena.expr(present.condition).kind,
                KlibIrExprKind::Const(KlibIrConstant::Boolean(true))
            )
        {
            return Err(mismatch(
                "a safe-call conditional has another type or else condition",
            ));
        }
        let (read, null) = self.elvis_null_test(absent.condition)?;
        let read = self.arena.expr(read);
        let null = self.arena.expr(null);
        let absent_result = self.arena.expr(absent.result);
        if !matches!(&read.kind, KlibIrExprKind::GetValue { symbol: read, .. } if *read == symbol)
            || self.ty(read, "safe-call temporary read")? != input
            || !matches!(null.kind, KlibIrExprKind::Const(KlibIrConstant::Null))
            || !matches!(
                absent_result.kind,
                KlibIrExprKind::Const(KlibIrConstant::Null)
            )
            || self.ty(absent_result, "safe-call null result")? != Ty::nullable(Ty::Nothing)
        {
            return Err(mismatch(
                "a safe-call null proof does not name its exact temporary",
            ));
        }
        let variable = self.unnamed_local(temporary)?;
        let local = self
            .locals
            .get(&symbol)
            .expect("the safe-call temporary was declared");
        let condition_read = self.ir.add_expr(IrExpr::GetValue(local.index));
        let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
        let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: condition_read,
            rhs: null,
        });
        let null_result = self.ir.add_expr(IrExpr::Const(IrConst::Null));
        let proof = NullProvenRead::Value(symbol.clone());
        self.null_proven.insert(proof.clone());
        let selector = self.expression(present.result);
        self.null_proven.remove(&proof);
        let selector = self.coercion(selector?, ty);
        let guarded = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(condition), null_result), (None, selector)],
        });
        self.ir.whens.exhaustive.insert(guarded, ty);
        self.ir.set_logical_type(guarded, ty);
        self.ir.null_guards.insert(guarded);
        let block = self.ir.add_expr(IrExpr::Block {
            stmts: vec![variable],
            value: Some(guarded),
        });
        self.ir.safe_calls.insert(block);
        Ok(self.typed(block, ty))
    }

    /// The selector's read of its safe-call temporary, cast to the non-null type its null test
    /// proves: an anonymous snapshot read under the conversion checked FIR lowering places there.
    /// `None` for any other cast.
    pub(super) fn null_proven_receiver(
        &mut self,
        argument: KlibIrExprId,
        target: Ty,
    ) -> Lowered<Option<ExprId>> {
        let read = self.arena.expr(argument);
        let KlibIrExprKind::GetValue { symbol, .. } = &read.kind else {
            return Ok(None);
        };
        if !self
            .null_proven
            .contains(&NullProvenRead::Value(symbol.clone()))
            || target.is_nullable()
        {
            return Ok(None);
        }
        let Some(local) = self.locals.get(symbol) else {
            return Ok(None);
        };
        let local_ty = local.ty;
        let local_index = local.index;
        if self.ty(read, "safe-call receiver read")? != local_ty {
            return Err(mismatch(
                "a safe-call receiver read is typed apart from its temporary",
            ));
        }
        if Ty::nullable(target) != local_ty {
            return Ok(None);
        }
        let read = self.ir.add_expr(IrExpr::GetValue(local_index));
        Ok(Some(self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: read,
            type_operand: target.canonical_semantic(),
        })))
    }

    /// The `null` proof the first of a compiler-generated `when`'s two branches gives its `else`:
    /// `when { read == null -> …; else -> … }` over a field read on a value.
    pub(super) fn generated_null_test(&self, condition: KlibIrExprId) -> Option<NullProvenRead> {
        let (read, null) = self.elvis_null_test(condition).ok()?;
        if !matches!(
            self.arena.expr(null).kind,
            KlibIrExprKind::Const(KlibIrConstant::Null)
        ) {
            return None;
        }
        NullProvenRead::field_read(self.arena, read)
    }

    /// `read`, already lowered as `lowered` of the nullable `actual`, as a value of the non-null
    /// `expected`, when a generated `when` proved this exact field read non-null: the smart cast
    /// checked FIR lowering places, to the definitely-non-null form of `actual` (`String` for
    /// `String?`, `T & Any` for a `T` whose bound admits `null`), which is then a value of each
    /// non-null supertype `expected` of that form.
    pub(super) fn null_proven_field(
        &mut self,
        read: KlibIrExprId,
        lowered: ExprId,
        actual: Ty,
        expected: Ty,
    ) -> Option<ExprId> {
        let non_null = actual.definitely_non_null();
        if expected.is_nullable() || non_null == actual {
            return None;
        }
        let proven = non_null.canonical_semantic();
        let proof = NullProvenRead::field_read(self.arena, read)?;
        if !self.null_proven.contains(&proof)
            || !(super::is_value_of(proven, expected) || self.is_supertype_of(expected, proven))
        {
            return None;
        }
        let cast = self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: lowered,
            type_operand: proven,
        });
        let cast = self.typed(cast, proven);
        if super::is_value_of(proven, expected) {
            return Some(cast);
        }
        self.upcast(cast, proven, expected)
    }
}
