//! Declaration references use the ordinary common-IR reference value and invocation adapter.

use super::*;
use crate::ir::{IrCallableReference, IrCallableReferenceTarget};
use crate::types::TyKind;

impl BodyLowering<'_> {
    pub(super) fn callable_reference(
        &mut self,
        reference: super::super::callable_references::UnboundReference<'_>,
        nominal: Ty,
    ) -> Lowered<ExprId> {
        let TyKind::Obj(_, arguments) = nominal.kind() else {
            return Err(mismatch(
                "a declaration reference without its nominal function type",
            ));
        };
        if arguments.len() != reference.arity + 1 {
            return Err(mismatch(
                "a declaration reference with an inconsistent type application",
            ));
        }
        let function_type = Ty::fun(
            arguments[..reference.arity].to_vec(),
            arguments[reference.arity],
        );
        let function = self.arena.function(reference.invoke);
        let header = super::super::function_lowering::closure_header(
            self.arena,
            function,
            self.header,
            &[],
        )?;
        let linked = self.linker.link_reference(self.ir, reference.target)?;
        let declared = &linked.call;
        if linked.kind != crate::libraries::ExternalCallableKind::TopLevel
            || header.params() != &arguments[..reference.arity]
            || header.result() != arguments[reference.arity]
            || declared.params != header.params()
            || declared.ret != header.result()
            || declared.receiver.is_some()
            || declared.context_count != 0
            || declared.singleton_dispatch.is_some()
            || !declared.type_parameters.is_empty()
            || !declared.class_type_parameters.is_empty()
            || matches!(
                declared.callee,
                Callee::External {
                    extension_receiver_parameter: Some(_),
                    ..
                }
            )
        {
            return unsupported("an adapted, generic or receiver-bearing declaration reference");
        }
        let declaration = linked.declaration;
        let declaration_parameters = declared.params.clone().into_boxed_slice();
        let declaration_result = declared.ret;
        let adapter = super::super::function_lowering::declare_function(self.ir, &header);
        self.ir.lambda_own_params_from.insert(adapter, 0);
        self.ir
            .set_method_visibility(adapter, crate::types::Visibility::Private);
        // Admission proved this exact Return(Call(target, own parameters in order)) wrapper.
        // Use ordinary checked-call lowering directly, retaining the declaration edge even for
        // a target whose implementation is a compiler-provided operation.
        let KlibIrExprKind::Return { value, .. } = &self.arena.expr(reference.returned).kind else {
            unreachable!("the shared reference admission proved a return")
        };
        let KlibIrExprKind::Call { access, .. } = &self.arena.expr(*value).kind else {
            unreachable!("the shared reference admission proved a forwarding call")
        };
        let (body, result) = {
            let mut lowering = BodyLowering::new(
                self.arena,
                LoweredFunction {
                    symbol: &function.base.symbol,
                    function: adapter,
                },
                &header,
                self.builtins,
                self.ir,
                self.linker,
            );
            let call = lowering.linked_call(linked.call, access, declaration_result)?;
            let result = lowering.typed(call, declaration_result);
            let returned = lowering.ir.add_expr(IrExpr::Return(Some(result)));
            lowering.ir.checked_return_depths.insert(returned, 0);
            let returned = lowering.typed(returned, Ty::Nothing);
            let body = lowering.ir.add_expr(IrExpr::Block {
                stmts: vec![returned],
                value: None,
            });
            let body = lowering.typed(body, Ty::Nothing);
            (body, result)
        };
        self.ir.callable_scopes.insert(body);
        self.ir
            .callable_reference_adapter_results
            .insert(body, result);
        self.ir.functions[adapter as usize].body = Some(body);
        Ok(self
            .ir
            .add_expr(IrExpr::CallableReference(Box::new(IrCallableReference {
                target: IrCallableReferenceTarget::External {
                    declaration,
                    receiver: None,
                },
                adapter,
                captures: Vec::new(),
                bound_receiver: None,
                function_type,
                declaration_parameters,
                declaration_result,
                declaration_suspend: false,
                adaptation: None,
                reflection_owner: None,
            }))))
    }
}
