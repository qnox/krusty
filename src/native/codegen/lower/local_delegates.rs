//! Checked local delegated-property conventions.
//!
//! Common lowering records each selected accessor as a closed expression template with an exact
//! parameter/result contract. Native realizes that contract as a file-local function. The helper
//! boundary matters: an inline convention body can contain a `Return`, which must return from the
//! accessor and never from the source function that reads the delegated property.

use super::*;

impl FileLowering<'_> {
    pub(super) fn declare_local_delegate_helpers(&mut self) -> Result<(), Unsupported> {
        for (plan, declaration) in self.ir.local_delegate_plans.iter().enumerate() {
            let plan = plan as u32;
            for (setter, accessor) in std::iter::once((false, &declaration.getter))
                .chain(declaration.setter.as_ref().map(|accessor| (true, accessor)))
            {
                let side = if setter { "set" } else { "get" };
                let symbol = format!("kt_local_delegate_{plan}_{side}");
                let function =
                    self.declare_local_function(&symbol, &accessor.parameters, accessor.result)?;
                self.local_delegate_helpers.insert((plan, setter), function);
            }
        }
        Ok(())
    }

    pub(super) fn define_local_delegate_helpers(&mut self) -> Result<(), Unsupported> {
        for plan in 0..self.ir.local_delegate_plans.len() as u32 {
            let declaration = &self.ir.local_delegate_plans[plan as usize];
            let mut accessors = vec![(false, declaration.getter.clone())];
            accessors.extend(
                declaration
                    .setter
                    .as_ref()
                    .cloned()
                    .map(|accessor| (true, accessor)),
            );
            for (setter, accessor) in accessors {
                let function = self.local_delegate_helpers[&(plan, setter)];
                let side = if setter { "set" } else { "get" };
                let symbol = format!("kt_local_delegate_{plan}_{side}");
                let signature = self.signature_of(&accessor.parameters, accessor.result)?;
                self.emit_function(
                    function,
                    signature,
                    accessor.result,
                    &symbol,
                    &mut |body, parameters| {
                        body.unresolved_reified.extend(
                            accessor
                                .type_parameters
                                .iter()
                                .filter(|parameter| parameter.reified)
                                .map(|parameter| parameter.semantic_name.clone()),
                        );
                        for (slot, (&value, &ty)) in parameters
                            .iter()
                            .zip(accessor.parameters.iter())
                            .enumerate()
                        {
                            let variable = body.declare_value(slot as u32, ty)?;
                            body.builder.def_var(variable, value);
                        }
                        if accessor.result == Ty::Unit {
                            return body.statement(accessor.body);
                        }
                        let value = body.coerce(accessor.body, accessor.result)?;
                        if body.terminated {
                            return Ok(());
                        }
                        let Some(value) = value else {
                            return Err(
                                "a local delegate accessor returned a `Unit` value".to_string()
                            );
                        };
                        body.builder.ins().return_(&[value]);
                        body.terminate();
                        Ok(())
                    },
                )?;
            }
        }
        Ok(())
    }
}

impl BodyLowering<'_, '_, '_> {
    pub(super) fn local_delegate_access(
        &mut self,
        access: crate::ir::IrLocalDelegateAccess,
    ) -> Result<Option<Value>, Unsupported> {
        let plan = self
            .file
            .ir
            .local_delegate_plans
            .get(access.plan as usize)
            .ok_or_else(|| {
                "a local delegated-property access without its checked plan".to_string()
            })?;
        let setter = access.value.is_some();
        let accessor = match setter {
            false => &plan.getter,
            true => plan.setter.as_ref().ok_or_else(|| {
                "a write through a read-only local delegate convention".to_string()
            })?,
        };

        // The enclosing receiver precedes the delegate for a member-extension convention; a
        // setter value follows it. These are the exact coordinates in `accessor.parameters`.
        let mut operands = Vec::with_capacity(
            usize::from(access.dispatch_receiver.is_some())
                + 1
                + usize::from(access.value.is_some()),
        );
        operands.extend(access.dispatch_receiver);
        operands.push(access.delegate);
        operands.extend(access.value);
        if operands.len() != accessor.parameters.len() {
            return Err("a local delegated-property access with malformed operands".to_string());
        }
        let arguments = self.arguments(&operands, &accessor.parameters)?;
        if self.terminated {
            return Ok(None);
        }
        let function = self
            .file
            .local_delegate_helpers
            .get(&(access.plan, setter))
            .copied()
            .ok_or_else(|| "an undeclared local delegate accessor helper".to_string())?;
        let reference = self.func_ref(function);
        let call = self.emit_call(reference, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }
}
