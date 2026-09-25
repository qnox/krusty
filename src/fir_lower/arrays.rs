//! Backend-neutral lowering of checker-selected array constructions.

use super::inlining::LambdaParameterBinding;
use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    pub(super) fn array_literal(
        &mut self,
        array_type: crate::fir::ResolvedTy,
        elements: &[crate::fir::FirArrayElement],
    ) -> Result<crate::ir::ExprId, FirLoweringFailure> {
        let elements = elements
            .iter()
            .map(|element| {
                Ok((
                    self.expression_with_conversion(element.value, element.conversion)?,
                    element.spread,
                ))
            })
            .collect::<Result<Vec<_>, FirLoweringFailure>>()?;
        let (values, spreads): (Vec<_>, Vec<_>) = elements.into_iter().unzip();
        Ok(self.ir.add_expr(crate::ir::IrExpr::Vararg {
            array_type: array_type.get(),
            spreads,
            elements: values,
        }))
    }

    pub(super) fn array_construction(
        &mut self,
        array_type: crate::fir::ResolvedTy,
        element_type: crate::fir::ResolvedTy,
        size: crate::fir::FirExprId,
        size_conversion: Option<crate::fir::FirConversion>,
        initializer: Option<crate::fir::FirExprId>,
    ) -> Result<crate::ir::ExprId, FirLoweringFailure> {
        let size = self.expression_with_conversion(size, size_conversion)?;
        let initializer = initializer
            .map(|initializer| {
                let initializer_type = self
                    .body
                    .expr(initializer)
                    .ok_or(FirLoweringFailure::MissingExpression(initializer))?
                    .ty
                    .get();
                let value = self.expression(initializer)?;
                Ok::<_, FirLoweringFailure>((value, initializer_type))
            })
            .transpose()?;

        self.array_construction_from_values(array_type, element_type, size, initializer)
    }

    /// Lower `Array(size, init)` (and the primitive array constructors) as kotlinc's
    /// `ArrayConstructorLowering` writes it:
    ///
    /// ```text
    /// var index = 0
    /// val size = <size>                  // dropped when <size> is a constant or a stable read
    /// val result = <new array>(size)
    /// val init = <init>                  // not a lambda: dropped when it is a stable read
    /// while (index < size) { val tempIndex = index; result[tempIndex] = init(tempIndex); index++ }
    /// result
    /// ```
    ///
    /// A lambda initializer is spliced into the loop with its parameter remapped onto `tempIndex`,
    /// so it has no local of its own, and it reads what it captures where that value lives.
    /// `JvmOptimizationLowering` removes the `size` and `init` temporaries whose initializer is a
    /// constant or a read of an immutable binding; lowering simply does not create them.
    pub(super) fn array_construction_from_values(
        &mut self,
        array_type: crate::fir::ResolvedTy,
        element_type: crate::fir::ResolvedTy,
        size: crate::ir::ExprId,
        initializer: Option<(crate::ir::ExprId, crate::types::Ty)>,
    ) -> Result<crate::ir::ExprId, FirLoweringFailure> {
        use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrIntrinsic};
        use crate::types::Ty;

        let Some((initializer, initializer_ty)) = initializer else {
            return Ok(self.ir.add_expr(IrExpr::NewArray {
                array_type: array_type.get(),
                size,
            }));
        };

        let mut statements = Vec::new();
        let index_slot = self.allocate_temporary();
        let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        statements.push(self.ir.add_expr(IrExpr::Variable {
            index: index_slot,
            ty: Ty::Int,
            init: Some(zero),
            named: false,
        }));

        let size = self.reusable_operand(size, Ty::Int, &mut statements);
        let allocation_size = self.read_operand(&size);
        let allocation = self.ir.add_expr(IrExpr::NewArray {
            array_type: array_type.get(),
            size: allocation_size,
        });
        let array_slot = self.allocate_temporary();
        statements.push(self.ir.add_expr(IrExpr::Variable {
            index: array_slot,
            ty: array_type.get(),
            init: Some(allocation),
            named: false,
        }));

        let initializer = match self.ir.expr(initializer).clone() {
            IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                sam,
                inline_body: Some(inline_body),
            } => {
                let capture_types = self
                    .ir
                    .functions
                    .get(impl_fn as usize)
                    .and_then(|function| function.params.get(..captures.len()))
                    .map(<[Ty]>::to_vec)
                    .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall)?;
                // kotlinc's lambda reads its captured values in place. A capture that is not a
                // plain value read still has to be evaluated once, before the loop.
                let captures = captures
                    .into_iter()
                    .zip(capture_types)
                    .map(|(capture, ty)| {
                        if matches!(self.ir.expr(capture), IrExpr::GetValue(_)) {
                            capture
                        } else {
                            let slot = self.hold_operand(capture, ty, &mut statements);
                            self.ir.add_expr(IrExpr::GetValue(slot))
                        }
                    })
                    .collect();
                self.ir.exprs[initializer as usize] = IrExpr::Lambda {
                    impl_fn,
                    arity,
                    captures,
                    sam,
                    inline_body: Some(inline_body),
                };
                ArrayInitializer::Inline(initializer)
            }
            _ => ArrayInitializer::Invoked(self.reusable_operand(
                initializer,
                initializer_ty,
                &mut statements,
            )),
        };

        let index = self.ir.add_expr(IrExpr::GetValue(index_slot));
        let bound = self.read_operand(&size);
        let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Lt,
            lhs: index,
            rhs: bound,
        });
        let element_index_slot = self.allocate_temporary();
        let index = self.ir.add_expr(IrExpr::GetValue(index_slot));
        let element_index = self.ir.add_expr(IrExpr::Variable {
            index: element_index_slot,
            ty: Ty::Int,
            init: Some(index),
            named: false,
        });
        let (function, inline) = match &initializer {
            ArrayInitializer::Inline(lambda) => (*lambda, true),
            ArrayInitializer::Invoked(operand) => (self.read_operand(operand), false),
        };
        let argument = self.ir.add_expr(IrExpr::GetValue(element_index_slot));
        let value = self.ir.add_expr(IrExpr::InvokeFunction {
            func: function,
            args: vec![argument],
            params: vec![Ty::Int],
            ret: element_type.get(),
        });
        if inline {
            self.splice_inline_lambda(value, LambdaParameterBinding::Remapped)
                .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall)?;
        }
        let array = self.ir.add_expr(IrExpr::GetValue(array_slot));
        let element = self.ir.add_expr(IrExpr::GetValue(element_index_slot));
        let set = self.ir.add_expr(IrExpr::Call {
            callee: Callee::Intrinsic {
                operation: IrIntrinsic::ArraySet,
                ret: Ty::Unit,
            },
            dispatch_receiver: Some(array),
            args: vec![element, value],
        });
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![element_index, set],
            value: None,
        });
        let index = self.ir.add_expr(IrExpr::GetValue(index_slot));
        let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let increment = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: index,
            rhs: one,
        });
        let update = self.ir.add_expr(IrExpr::SetValue {
            var: index_slot,
            value: increment,
        });
        statements.push(self.ir.add_expr(IrExpr::While {
            cond: condition,
            body,
            update: Some(update),
            post_test: false,
            label: None,
        }));

        let result = self.ir.add_expr(IrExpr::GetValue(array_slot));
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: Some(result),
        }))
    }

    /// `value` as an operand read more than once: a constant or a stable read is read again where
    /// it is used, anything else is evaluated once into a temporary appended to `statements`.
    fn reusable_operand(
        &mut self,
        value: crate::ir::ExprId,
        ty: crate::types::Ty,
        statements: &mut Vec<crate::ir::ExprId>,
    ) -> ReusableOperand {
        if let Some(slot) = self.stable_value_read(value) {
            return ReusableOperand::Value(slot);
        }
        if let Some(constant) = integral_constant(self.ir, value) {
            return ReusableOperand::Constant(constant);
        }
        ReusableOperand::Value(self.hold_operand(value, ty, statements))
    }

    /// Evaluate `value` once into a new temporary declared in `statements`.
    fn hold_operand(
        &mut self,
        value: crate::ir::ExprId,
        ty: crate::types::Ty,
        statements: &mut Vec<crate::ir::ExprId>,
    ) -> u32 {
        let slot = self.allocate_temporary();
        statements.push(self.ir.add_expr(crate::ir::IrExpr::Variable {
            index: slot,
            ty,
            init: Some(value),
            named: false,
        }));
        slot
    }

    fn read_operand(&mut self, operand: &ReusableOperand) -> crate::ir::ExprId {
        self.ir.add_expr(match operand {
            ReusableOperand::Value(slot) => crate::ir::IrExpr::GetValue(*slot),
            ReusableOperand::Constant(constant) => crate::ir::IrExpr::Const(constant.clone()),
        })
    }
}

enum ArrayInitializer {
    /// A lambda spliced into the loop.
    Inline(crate::ir::ExprId),
    /// A function value invoked for every element.
    Invoked(ReusableOperand),
}

/// An operand read at several places without being evaluated again.
enum ReusableOperand {
    Value(u32),
    Constant(crate::ir::IrConst),
}

/// The `Int` constant `value` is, directly or under the implicit coercion a literal argument
/// arrives in.
fn integral_constant(
    ir: &crate::ir::IrFile,
    value: crate::ir::ExprId,
) -> Option<crate::ir::IrConst> {
    match ir.expr(value) {
        crate::ir::IrExpr::Const(constant @ crate::ir::IrConst::Int(_)) => Some(constant.clone()),
        crate::ir::IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            type_operand: crate::types::Ty::Int,
        } => integral_constant(ir, *arg),
        _ => None,
    }
}
