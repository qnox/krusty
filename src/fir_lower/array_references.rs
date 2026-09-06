//! Materialization of references to checker-selected compiler array factories.

use crate::fir::{
    FirAdaptedReferenceArgument, FirCallableReferenceBinding, FirReferenceAdaptation, ResolvedTy,
};
use crate::ir::{ExprId, IrExpr, IrFunction};
use crate::types::{ArrayFactoryKind, Ty};

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn checked_array_factory_reference(
        &mut self,
        operation: ArrayFactoryKind,
        array_type: ResolvedTy,
        element_type: ResolvedTy,
        parameters: &[ResolvedTy],
        binding: FirCallableReferenceBinding,
        adaptation: Option<&FirReferenceAdaptation>,
        reference_ty: Ty,
        reflective: bool,
    ) -> Result<ExprId, FirLoweringFailure> {
        let Ty::Fun(reference) = reference_ty.non_null() else {
            return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
        };
        if binding != FirCallableReferenceBinding::Static || reflective {
            return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
        }

        let (vararg_factory, nullable_factory) = match operation {
            ArrayFactoryKind::PrimitiveVararg(element) => {
                if element != element_type.get() {
                    return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
                }
                (true, false)
            }
            ArrayFactoryKind::ReferenceVararg => (true, false),
            ArrayFactoryKind::EmptyReference => (false, false),
            ArrayFactoryKind::NullableReference => (false, true),
            ArrayFactoryKind::PrimitiveSize(_) | ArrayFactoryKind::ReferenceSize => {
                return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
            }
        };
        if parameters.len() != usize::from(vararg_factory || nullable_factory)
            || nullable_factory && parameters[0].get() != Ty::Int
        {
            return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
        }

        // The adapter owns a fresh value-slot namespace. Sized factories allocate temporaries in
        // that namespace; never let those indices inherit the enclosing source body's counter.
        let enclosing_temporary = self.next_temporary;
        self.next_temporary = u32::try_from(reference.params.len())
            .map_err(|_| FirLoweringFailure::UnsupportedIntrinsicCall)?;
        let value = (|| {
            if let Some(adaptation) = adaptation {
                if adaptation.arguments.len() != parameters.len()
                    || !adaptation
                        .parameter_types
                        .iter()
                        .map(|parameter| parameter.get())
                        .eq(reference.params.iter().copied())
                    || adaptation.result_type.get() != reference.ret
                {
                    return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
                }
                match adaptation.arguments.as_ref() {
                    [] if !vararg_factory && !nullable_factory => {
                        Ok(self.ir.add_expr(IrExpr::Vararg {
                            array_type: array_type.get(),
                            spreads: Vec::new(),
                            elements: Vec::new(),
                        }))
                    }
                    [FirAdaptedReferenceArgument::Value(source)] if nullable_factory => {
                        if reference.params.get(*source as usize).is_none() {
                            return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
                        }
                        let size = self.ir.add_expr(IrExpr::GetValue(*source));
                        self.array_construction_from_values(array_type, element_type, size, None)
                    }
                    [FirAdaptedReferenceArgument::Value(source)] if vararg_factory => {
                        if reference.params.get(*source as usize).is_none() {
                            return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
                        }
                        Ok(self.ir.add_expr(IrExpr::GetValue(*source)))
                    }
                    [FirAdaptedReferenceArgument::Vararg {
                        values,
                        whole_array: true,
                    }] if vararg_factory && values.len() == 1 => {
                        let source = values[0];
                        if reference.params.get(source as usize).is_none() {
                            return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
                        }
                        // A callable reference exposes a vararg declaration's array parameter as one
                        // ordinary function parameter. Passing that whole array invokes the selected
                        // factory with the same backing array; it is not source `*spread` syntax.
                        Ok(self.ir.add_expr(IrExpr::GetValue(source)))
                    }
                    [FirAdaptedReferenceArgument::Vararg {
                        values,
                        whole_array: false,
                    }] if vararg_factory => {
                        let elements = values
                            .iter()
                            .copied()
                            .map(|source| {
                                reference
                                    .params
                                    .get(source as usize)
                                    .map(|_| self.ir.add_expr(IrExpr::GetValue(source)))
                                    .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall)
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok(self.ir.add_expr(IrExpr::Vararg {
                            array_type: array_type.get(),
                            spreads: vec![false; elements.len()],
                            elements,
                        }))
                    }
                    _ => Err(FirLoweringFailure::UnsupportedIntrinsicCall),
                }
            } else if nullable_factory {
                if reference.params.as_slice() != [Ty::Int] {
                    Err(FirLoweringFailure::UnsupportedIntrinsicCall)
                } else {
                    let size = self.ir.add_expr(IrExpr::GetValue(0));
                    self.array_construction_from_values(array_type, element_type, size, None)
                }
            } else if vararg_factory {
                if reference.params.len() != 1 {
                    Err(FirLoweringFailure::UnsupportedIntrinsicCall)
                } else {
                    Ok(self.ir.add_expr(IrExpr::GetValue(0)))
                }
            } else {
                if !reference.params.is_empty() {
                    Err(FirLoweringFailure::UnsupportedIntrinsicCall)
                } else {
                    Ok(self.ir.add_expr(IrExpr::Vararg {
                        array_type: array_type.get(),
                        spreads: Vec::new(),
                        elements: Vec::new(),
                    }))
                }
            }
        })();
        self.next_temporary = enclosing_temporary;
        let value = value?;

        let body = self.callable_reference_adapter_body(value, array_type.get(), reference.ret);
        let function = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_array_factory_ref_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: reference.params.to_vec(),
            ret: crate::types::stored_value_ty(reference.ret),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        if adaptation.is_some_and(|adaptation| adaptation.suspend_conversion) {
            self.ir.suspend_funs.push(function);
        }
        self.ir.private_methods.insert(function);
        self.ir.lambda_own_params_from.insert(function, 0);
        let arity = u8::try_from(reference.params.len())
            .map_err(|_| FirLoweringFailure::UnsupportedIntrinsicCall)?;
        Ok(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: function,
            arity,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        }))
    }
}
