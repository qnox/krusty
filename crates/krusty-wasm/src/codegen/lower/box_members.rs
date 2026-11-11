//! Calls of a library class's member that the boxes below it implement (`Number.toLong()` on a
//! `Number`), through the member's stub (`crate::boxes`).
//!
//! Checked FIR froze, on the selected declaration, what each primitive whose classifier is below
//! the member's runs for it. A member of `kotlin.Any` is realized through the receiver's own
//! representation instead (`super::any_members`), and an interface's member has implementations
//! beside the boxes, so neither goes through here.

use krusty::backend::{BackendCallableFact, CheckedBackendCallables};

use super::*;

/// Whether the dependency call `expression` of `fact` dispatches to the boxes implementing it.
pub(in crate::codegen) fn dispatches_to_boxes(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    expression: u32,
    fact: &BackendCallableFact,
) -> bool {
    !fact.primitive_implementations.is_empty()
        && !fact.owner_is_interface
        && any_members::call_member(ir, callables, expression).is_none()
}

impl Body<'_> {
    /// Emit `id` when it is a call through a library class member the boxes implement.
    pub(super) fn box_member_expression(
        &mut self,
        id: u32,
        node: &IrExpr,
    ) -> Option<Result<(), Unsupported>> {
        let IrExpr::Call {
            callee,
            dispatch_receiver: Some(receiver),
            args,
        } = node
        else {
            return None;
        };
        let Callee::External {
            target,
            defaults,
            extension_receiver_parameter: None,
            ..
        } = &**callee
        else {
            return None;
        };
        let fact = self.callables.callable(*target)?;
        if !defaults.is_empty() || !dispatches_to_boxes(self.ir, self.callables, id, fact) {
            return None;
        }
        Some(self.box_member_call(id, fact.clone(), *receiver, args))
    }

    fn box_member_call(
        &mut self,
        id: u32,
        fact: BackendCallableFact,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let identity = fact
            .dependency_callable
            .as_ref()
            .ok_or("a library class member without its KLIB identity")?;
        let params = fact.declared_params.as_deref().unwrap_or(&fact.params);
        let result = fact.declared_ret.unwrap_or(fact.ret);
        if params.len() != args.len() {
            return Err("a call whose arguments do not match its parameters".into());
        }
        let signature = super::super::interface_members::member_signature(params, result)?;
        let member = self.runtime.box_member(
            self.module,
            identity,
            signature,
            &fact.primitive_implementations,
        )?;
        self.value(receiver, objects::any())?;
        for (&arg, &param) in args.iter().zip(params) {
            self.value(arg, param)?;
        }
        let stub = self.runtime.objects.interfaces.stub(self.module, member);
        self.code.call(stub);
        match self.type_of(id)? {
            Some(ty) => self.coerce_emitted_value(result, ty),
            None => {
                self.code.unreachable();
                Ok(())
            }
        }
    }
}
