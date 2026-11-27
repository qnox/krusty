//! Top-level functions no provider records, linked from their serialized headers: one private
//! to its file, and an internal one its module's metadata does not publish, as a function a
//! compiler plugin generates after the metadata is written.

use super::*;

impl Linker<'_, '_> {
    /// The internal top-level function of this module that `callee` names when neither the
    /// module's metadata nor the active selection records it, as for a function a compiler plugin
    /// adds after metadata is written: it links as a file-private function does.
    pub(super) fn unpublished_internal_function(
        &self,
        callee: &KlibIrSignature,
    ) -> Option<KlibDependencyCallable> {
        let KlibIrSignature::Public(signature) = callee else {
            return None;
        };
        let identity = KlibDependencyCallable {
            module: self.active_module.clone(),
            declaration: KlibDeclarationSignature::Public(signature.clone()),
        };
        (self
            .sources
            .bodies
            .is_internal_top_level_function(&identity)
            && self.sources.callees.get(&identity).is_none()
            && !self.sources.externals.contains_key(&identity)
            && !self.root.is_some_and(|root| &identity == root.identity())
            && self
                .sources
                .declarations
                .external_callable_by_dependency(&identity)
                .is_none())
        .then_some(identity)
    }

    /// The top-level function private to the active body's file that `identity` names. No
    /// provider records one: the unit emits it from its serialized header alone, as a function of
    /// the unit that it publishes to no one.
    pub(super) fn link_file_private(
        &mut self,
        ir: &mut IrFile,
        signature: KlibDeclarationSignature,
        identity: KlibDependencyCallable,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let callee_declined = |reason| {
            KlibBodyDeclineReason::CalleeDeclined(Box::new(KlibBodyDecline::new(
                signature.clone(),
                reason,
            )))
        };
        let (arena, declaration) = self
            .sources
            .bodies
            .function(&identity)
            .ok_or_else(|| callee_declined(KlibBodyDeclineReason::Unjoined))?;
        // A generic one binds the formals its own identity mints, as a provider's record binds a
        // published declaration's.
        let (mut types, type_parameters, type_parameter_bounds) =
            super::super::function_lowering::file_private_formals(&identity, arena, declaration)
                .map_err(callee_declined)?;
        super::super::private_signature_classes::bind(
            &mut types,
            self.sources.bodies,
            &identity.module,
            arena,
            declaration,
        )
        .map_err(callee_declined)?;
        self.private_members.entry(identity.clone()).or_default();
        let function = match self.functions.get(&identity) {
            Some(function) => *function,
            None => self
                .declare_with_types(ir, &identity, None, arena, declaration, types)
                .map_err(callee_declined)?,
        };
        let parameter_defaults = parameter_defaults(declaration);
        let declared = &ir.functions[function as usize];
        Ok(LinkedCallee {
            callee: Callee::Local(function),
            receiver: None,
            params: declared.params.clone(),
            ret: declared.ret,
            context_count: ir.declared_parameters(function).context_count,
            singleton_dispatch: None,
            type_parameters,
            type_parameter_bounds: Some(type_parameter_bounds),
            class_type_parameters: Vec::new(),
            parameter_defaults,
            edge: super::super::CallEdge::default(),
        })
    }
}
