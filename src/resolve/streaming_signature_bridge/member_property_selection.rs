//! Selection-time result and publication facts for a qualified member-property read.

use super::ProductionSignatureSemantics;
use crate::types::Ty;

impl ProductionSignatureSemantics<'_> {
    pub(super) fn selected_member_property_type(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        spelling: &str,
        publication_origin: Option<crate::fir::OriginId>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let implicit_receivers = self.implicit_receivers(scope);
        let selected = self.with_resolver(scope, |resolver| {
            resolver.select_member_property_applicable_where(receiver, spelling, |property| {
                let context_types = property.getter.params.get(..property.context_count)?;
                super::super::context_argument_types(
                    &implicit_receivers,
                    context_types,
                    &crate::symbol_resolver::SourceOracle(&source),
                )
                .map(|_| (true, property.context_count))
            })
        });
        crate::trace_compiler!(
            "signature",
            "member property selection receiver={receiver:?} name={spelling} result={:?}",
            selected.as_ref().map(|selected| selected.ty),
        );
        let selected = match selected {
            Ok(selected) => selected,
            Err(_) => {
                let extension = self.with_resolver(scope, |resolver| {
                    resolver
                        .select_extension_property(receiver, spelling)
                        .ok()
                        .flatten()
                });
                let Ok(extension) = extension else {
                    return Ok(None);
                };
                self.commit_postponed_property_receiver(scope, &extension, receiver);
                let publication = publication_origin.map(|_| {
                    self.selected_callable_publication(&extension.getter, Box::new([]), &[], false)
                });
                let finish = |result: crate::fir::ResolvedTy| {
                    if let (Some(origin), Some(publication)) =
                        (publication_origin, publication.clone())
                    {
                        self.record_final_selected_publication(origin, publication, result);
                    }
                    result
                };
                if let Some(source) = extension.source_key {
                    if let Some(signature) = self.demanded_source_signature(
                        Some(scope),
                        extension.stable_declaration,
                        demand,
                    )? {
                        return self
                            .apply_demanded_source_property(source, receiver, &signature)
                            .map(finish)
                            .map(Some);
                    }
                }
                return crate::fir::ResolvedTy::new(extension.ty)
                    .map(finish)
                    .map(Some)
                    .map_err(|_| Self::failure());
            }
        };
        let publication = publication_origin.and_then(|_| {
            let member = selected.read(receiver)?;
            Some(self.selected_member_publication(
                &member.member,
                selected.declaration_receiver,
                Box::new([]),
                &[],
                false,
            ))
        });
        let finish = |result: crate::fir::ResolvedTy| {
            if let (Some(origin), Some(publication)) = (publication_origin, publication.clone()) {
                self.record_final_selected_publication(origin, publication, result);
            }
            result
        };
        if let Some(property) = selected.property.as_ref() {
            if let Some(declaration) = property.stable_declaration {
                if self
                    .headers
                    .stub(declaration)
                    .is_some_and(|stub| stub.signature_inference.is_some())
                {
                    let result = self
                        .apply_dispatch_owner(
                            receiver,
                            property.owner,
                            None,
                            demand(declaration)?.result.get(),
                        )
                        .projection_read_ty();
                    return crate::fir::ResolvedTy::new(result)
                        .map(finish)
                        .map(Some)
                        .map_err(|_| Self::failure());
                }
            }
            if let Some(signature) =
                self.demanded_member_signature(property.stable_declaration, demand)?
            {
                let result = self
                    .apply_dispatch_owner(receiver, property.owner, None, signature.result.get())
                    .projection_read_ty();
                return crate::fir::ResolvedTy::new(result)
                    .map(finish)
                    .map(Some)
                    .map_err(|_| Self::failure());
            }
        }
        crate::fir::ResolvedTy::new(selected.ty)
            .map(finish)
            .map(Some)
            .map_err(|_| Self::failure())
    }
}
