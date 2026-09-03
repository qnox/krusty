//! Backend-neutral lowering of checker-selected array constructions.

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

    pub(super) fn array_construction_from_values(
        &mut self,
        array_type: crate::fir::ResolvedTy,
        element_type: crate::fir::ResolvedTy,
        size: crate::ir::ExprId,
        initializer: Option<(crate::ir::ExprId, crate::types::Ty)>,
    ) -> Result<crate::ir::ExprId, FirLoweringFailure> {
        use crate::ir::{Callee, IrBinOp, IrConst, IrExpr, IrIntrinsic};
        use crate::types::Ty;

        enum Initializer {
            Stored(u32),
            Inline(crate::ir::ExprId),
        }

        let size_slot = self.allocate_temporary();
        let size_declaration = self.ir.add_expr(IrExpr::Variable {
            index: size_slot,
            ty: Ty::Int,
            init: Some(size),
            named: false,
        });
        let mut statements = vec![size_declaration];
        let initializer = if let Some((value, ty)) = initializer {
            let inline_lambda = match self.ir.expr(value).clone() {
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
                    let mut stored_captures = Vec::with_capacity(captures.len());
                    for (capture, capture_ty) in captures.into_iter().zip(capture_types) {
                        // Array(size, init) is inline. Evaluate each captured operand once, at the
                        // lambda argument's source position, then let the structural inline splice
                        // read that stable slot on every loop iteration. This preserves closure
                        // construction order without materializing an ordinary function object.
                        let slot = self.allocate_temporary();
                        statements.push(self.ir.add_expr(IrExpr::Variable {
                            index: slot,
                            ty: capture_ty,
                            init: Some(capture),
                            named: false,
                        }));
                        stored_captures.push(self.ir.add_expr(IrExpr::GetValue(slot)));
                    }
                    self.ir.exprs[value as usize] = IrExpr::Lambda {
                        impl_fn,
                        arity,
                        captures: stored_captures,
                        sam,
                        inline_body: Some(inline_body),
                    };
                    Some(value)
                }
                _ => None,
            };
            if let Some(lambda) = inline_lambda {
                Some(Initializer::Inline(lambda))
            } else {
                let slot = self.allocate_temporary();
                statements.push(self.ir.add_expr(IrExpr::Variable {
                    index: slot,
                    ty,
                    init: Some(value),
                    named: false,
                }));
                Some(Initializer::Stored(slot))
            }
        } else {
            None
        };
        let size_read = self.ir.add_expr(IrExpr::GetValue(size_slot));
        let allocation = self.ir.add_expr(IrExpr::NewArray {
            array_type: array_type.get(),
            size: size_read,
        });
        let array_slot = self.allocate_temporary();
        let array_declaration = self.ir.add_expr(IrExpr::Variable {
            index: array_slot,
            ty: array_type.get(),
            init: Some(allocation),
            named: false,
        });
        statements.push(array_declaration);

        if let Some(initializer) = initializer {
            let index_slot = self.allocate_temporary();
            let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
            statements.push(self.ir.add_expr(IrExpr::Variable {
                index: index_slot,
                ty: Ty::Int,
                init: Some(zero),
                named: false,
            }));
            let index = self.ir.add_expr(IrExpr::GetValue(index_slot));
            let size = self.ir.add_expr(IrExpr::GetValue(size_slot));
            let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Lt,
                lhs: index,
                rhs: size,
            });
            let (initializer, inline) = match initializer {
                Initializer::Stored(slot) => (self.ir.add_expr(IrExpr::GetValue(slot)), false),
                Initializer::Inline(lambda) => (lambda, true),
            };
            let index = self.ir.add_expr(IrExpr::GetValue(index_slot));
            let value = self.ir.add_expr(IrExpr::InvokeFunction {
                func: initializer,
                args: vec![index],
                params: vec![Ty::Int],
                ret: element_type.get(),
            });
            if inline {
                self.splice_inline_lambda_invocation(value)
                    .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall)?;
            }
            let array = self.ir.add_expr(IrExpr::GetValue(array_slot));
            let index = self.ir.add_expr(IrExpr::GetValue(index_slot));
            let set = self.ir.add_expr(IrExpr::Call {
                callee: Callee::Intrinsic {
                    operation: IrIntrinsic::ArraySet,
                    ret: Ty::Unit,
                },
                dispatch_receiver: Some(array),
                args: vec![index, value],
            });
            let body = self.ir.add_expr(IrExpr::Block {
                stmts: vec![set],
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
        }

        let result = self.ir.add_expr(IrExpr::GetValue(array_slot));
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: Some(result),
        }))
    }
}
