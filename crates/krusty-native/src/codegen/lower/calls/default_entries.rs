//! Calls the shared default-argument lowering left in a form only this target can realize: the
//! physical symbol of another file's entry, a function's or a constructor's, and the value
//! receiver of a value-class member.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// A call of the default-argument entry another file of the module publishes under
    /// `provider`: the whole parameter frame, an absent operand in each omitted slot, then the
    /// mask words, exactly as the shared lowering arranged them.
    pub(super) fn module_default_entry_call(
        &mut self,
        provider: krusty::fir::CallableId,
        target: krusty::fir::CallableId,
        dispatch_receiver_ty: Option<Ty>,
        params: &[Ty],
        ret: Ty,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let symbol = super::super::super::super::symbols::module_default_symbol(
            self.file.module_name,
            provider,
        );
        let mut frame = dispatch_receiver_ty
            .into_iter()
            .chain(params.iter().copied())
            .collect::<Vec<_>>();
        frame.extend(std::iter::repeat_n(
            Ty::Int,
            krusty::backend::default_arguments::mask_words(params.len()),
        ));
        if args.len() != frame.len() {
            return Err(declined!(
                "a default-argument entry call with {} of {} operands (`{symbol}`)",
                args.len(),
                frame.len()
            ));
        }
        let id = self.file.import(&symbol, &frame, ret)?;
        let arguments = self.arguments(args, &frame)?;
        if self.terminated {
            return Ok(None);
        }
        // As for a call that supplies everything: operands first, then the defining file's
        // initializer, then the call, whose defaults may read that file's top-level state.
        self.initialize_defining_file(if dispatch_receiver_ty.is_some() {
            provider
        } else {
            target
        })?;
        if self.terminated {
            return Ok(None);
        }
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// A construction through the entry the file declaring the class publishes for the defaults
    /// of `constructor`: an inner class's enclosing instance, the whole parameter frame, an
    /// absent operand in each omitted slot, then the mask words, as the shared lowering arranged
    /// them.
    pub(super) fn module_construction_default_entry_call(
        &mut self,
        constructor: krusty::fir::DeclarationId,
        outer: Option<Ty>,
        params: &[Ty],
        ret: Ty,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let instance = krusty::backend::instance_links::InstanceEntry::ConstructorDefaults {
            module: self.file.module_name.into(),
            declaration: constructor,
        };
        let Some(symbol) = self.file.dependency_instances.get(&instance) else {
            return Err(declined!(
                "a cross-file construction leaving arguments to a constructor its file does not \
                 expose"
            ));
        };
        let mut frame = outer.into_iter().collect::<Vec<_>>();
        frame.extend_from_slice(params);
        frame.extend(std::iter::repeat_n(
            Ty::Int,
            krusty::backend::default_arguments::mask_words(params.len()),
        ));
        if args.len() != frame.len() {
            return Err(declined!(
                "a constructor default-argument entry call with {} of {} operands (`{symbol}`)",
                args.len(),
                frame.len()
            ));
        }
        let id = self.file.import(symbol, &frame, ret)?;
        let arguments = self.arguments(args, &frame)?;
        if self.terminated {
            return Ok(None);
        }
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// The `this` operand of a direct call of a value-class member, which takes the VALUE: the
    /// class is final, so nothing dispatches. `None` when evaluating the receiver terminated.
    pub(super) fn value_class_member_receiver(
        &mut self,
        function: u32,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let declaration = &self.file.ir.functions[function as usize];
        let Some(owner) = declaration
            .dispatch_receiver
            .filter(|&owner| self.file.values.is_value_class(owner))
        else {
            return Err(declined!(
                "a direct call of `{}` with a receiver",
                declaration.name
            ));
        };
        let value = self.coerce(receiver, Ty::obj_name(owner))?;
        if self.terminated {
            return Ok(None);
        }
        value
            .map(Some)
            .ok_or_else(|| declined!("a value-class receiver of no value"))
    }
}
