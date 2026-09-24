//! Stable declaration views shared by a checker's body-resolution queries.

use super::*;

impl Checker<'_> {
    pub(super) fn resolver(&self) -> crate::symbol_resolver::SymbolResolver<'_> {
        self.with_stable_symbol_records(
            crate::symbol_resolver::SymbolResolver::new_import_scoped_with_module(
                self.libraries,
                &self.module,
                &self.function_import_scope,
            ),
        )
        .with_access_context(
            self.source_package_name(),
            self.file_index,
            self.access_context_class_names(),
        )
    }

    pub(super) fn resolver_in_scope<'s>(
        &'s self,
        scope: &'s [TypeName],
    ) -> crate::symbol_resolver::SymbolResolver<'s> {
        self.with_stable_symbol_records(
            crate::symbol_resolver::SymbolResolver::new_scoped_with_module(
                self.libraries,
                &self.module,
                scope,
            ),
        )
        .with_access_context(
            self.source_package_name(),
            self.file_index,
            self.lexical_source_class_names(),
        )
    }

    /// A resolver over this checker's own federation (the module over the platform) shares the
    /// records `fed_source` memoizes once the declaration index is finalized. Pass 2 checks one
    /// bounded source unit against an immutable module and classpath, while mutable Pass-1 checks
    /// continue to read their providers directly.
    fn with_stable_symbol_records<'s>(
        &'s self,
        resolver: crate::symbol_resolver::SymbolResolver<'s>,
    ) -> crate::symbol_resolver::SymbolResolver<'s> {
        if self.resolved_index.is_some() {
            resolver.with_symbol_query_cache(&self.stable_federated_symbol_cache)
        } else {
            resolver
        }
    }

    /// Module-first symbol source used for receiver and extension ranking.
    pub(super) fn fed_source(&self) -> CachedCompositeSource<'_> {
        CachedCompositeSource::new(
            vec![
                &self.module as &dyn SymbolSource,
                self.libraries as &dyn SymbolSource,
            ],
            &self.stable_federated_symbol_cache,
        )
    }

    /// Receiver declarations from the finalized module and dependency providers. Body-local
    /// classifier overlays are deliberately unioned by their owning selectors after this query, so
    /// publishing a later local method cannot invalidate the stable portion cached here.
    pub(super) fn stable_receiver_callables(
        &self,
        receiver: Ty,
        name: &str,
    ) -> crate::libraries::Callables {
        if self.resolved_index.is_none() {
            return self.resolver().receiver_callables(receiver, name);
        }
        let key = (receiver, name.to_owned(), self.lexical_source_class_names());
        if let Some(callables) = self.stable_receiver_callable_cache.borrow().get(&key) {
            return callables.clone();
        }
        let callables = self.resolver().receiver_callables(receiver, name);
        self.stable_receiver_callable_cache
            .borrow_mut()
            .insert(key, callables.clone());
        callables
    }

    pub(super) fn stable_classifier_callable_signatures(&self, ty: Ty) -> Vec<Ty> {
        if self.resolved_index.is_none() {
            return crate::symbol_resolver::classifier_callable_signatures(&self.fed_source(), ty);
        }
        if let Some(signatures) = self.stable_callable_shape_cache.borrow().get(&ty) {
            return signatures.clone();
        }
        let signatures =
            crate::symbol_resolver::classifier_callable_signatures(&self.fed_source(), ty);
        self.stable_callable_shape_cache
            .borrow_mut()
            .insert(ty, signatures.clone());
        signatures
    }

    /// The unique current-module top-level declaration visible at this call-site rung.
    ///
    /// Pass 2 obtains this shape from finalized provider headers. In particular, it must not reopen
    /// the temporary Pass-1 signature graph merely to contextually type arguments before ordinary
    /// overload selection runs.
    pub(super) fn stable_single_top_level_signature(&self, name: &str) -> Option<Signature> {
        let mut declarations = self
            .resolver()
            .top_level_candidates(name)
            .into_iter()
            .filter(|function| function.stable_declaration.is_some());
        let declaration = declarations.next()?;
        declarations
            .next()
            .is_none()
            .then(|| signature_from_resolved_function(&declaration))
    }
}
