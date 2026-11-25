//! Materialization of references to checker-selected compiler array factories.

use crate::fir::{
    ExternalCallableId, FirAdaptedReferenceArgument, FirCallableReferenceBinding,
    FirReferenceAdaptation, ResolvedTy,
};
use crate::ir::{ExprId, IrExpr, IrFunction};
use crate::types::{ArrayFactoryKind, Ty, TyKind, TypeName};

use super::{BodyLowering, FirLoweringFailure};

/// The compiler array factory a reference names: the provider's declaration identity, the
/// construction it performs, and the checked types it constructs with.
pub(super) struct ArrayFactoryReference<'a> {
    pub(super) declaration: ExternalCallableId,
    pub(super) operation: ArrayFactoryKind,
    pub(super) array_type: ResolvedTy,
    pub(super) element_type: ResolvedTy,
    pub(super) parameters: &'a [ResolvedTy],
    pub(super) reflection_owner: Option<TypeName>,
}

impl BodyLowering<'_> {
    /// Materialize a reference to a compiler array factory as an ordinary dependency
    /// [`IrExpr::CallableReference`] to that declaration. Its adapter constructs the array the
    /// factory would, so no backend calls a method the factory does not have; the target backend
    /// chooses the carrier and reflects the declaration as for any other dependency reference.
    pub(super) fn checked_array_factory_reference(
        &mut self,
        factory: ArrayFactoryReference<'_>,
        binding: FirCallableReferenceBinding,
        adaptation: Option<&FirReferenceAdaptation>,
        reference_ty: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let ArrayFactoryReference {
            declaration,
            operation,
            array_type,
            element_type,
            parameters,
            reflection_owner,
        } = factory;
        let TyKind::Fun(reference) = reference_ty.non_null().kind() else {
            return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
        };
        if binding != FirCallableReferenceBinding::Static {
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

        let body = self
            .ir
            .callable_reference_adapter_body(value, array_type.get(), reference.ret);
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
        if reference.suspend {
            self.ir.suspend_funs.push(function);
        }
        self.ir
            .set_method_visibility(function, crate::types::Visibility::Private);
        self.ir.lambda_own_params_from.insert(function, 0);
        self.attach_generated_static_to_lexical_class(function);
        Ok(self.ir.add_expr(IrExpr::CallableReference(Box::new(
            crate::ir::IrCallableReference {
                target: crate::ir::IrCallableReferenceTarget::External {
                    declaration,
                    receiver: None,
                },
                adapter: function,
                captures: Vec::new(),
                bound_receiver: None,
                bound_contexts: 0,
                function_type: reference_ty,
                declaration_parameters: parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect(),
                declaration_result: array_type.get(),
                declaration_suspend: false,
                adaptation: adaptation.cloned().map(Box::new),
                reflection_owner,
            },
        ))))
    }
}
