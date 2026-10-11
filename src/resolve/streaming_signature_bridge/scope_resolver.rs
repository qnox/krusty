//! The resolver a signature scope reads: its file's imports and package, the lexically enclosing
//! classifiers it may access privately, the active postponed type variables, and its declaration's
//! lexical visibility-suppression policy. Every signature access check goes through this resolver,
//! so each reads the same policy.

use super::ProductionSignatureSemantics;

impl ProductionSignatureSemantics<'_> {
    pub(super) fn with_resolver<T>(
        &self,
        scope: crate::fir::SignatureScope,
        select: impl FnOnce(&crate::symbol_resolver::SymbolResolver<'_>) -> Option<T>,
    ) -> Result<T, crate::fir::DiagnosticId> {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let imports = self.function_import_scope(scope.source)?;
        let file = self
            .headers
            .scopes
            .file(scope.source)
            .ok_or_else(Self::failure)?;
        let package = self
            .headers
            .scopes
            .path(file.package)
            .iter()
            .map(|segment| self.headers.lookup_names.get(*segment))
            .collect::<Option<Vec<_>>>()
            .map(|segments| {
                if segments.is_empty() {
                    crate::types::TypeName::ROOT
                } else {
                    crate::types::type_name(&segments.join("/"))
                }
            })
            .ok_or_else(Self::failure)?;
        let mut lexical_classes = Vec::new();
        let mut owner = Some(scope.owner);
        while let Some(declaration) = owner {
            if let Some(classifier) = self.lexical_access_classifier(declaration) {
                lexical_classes.push(classifier);
            }
            owner = self
                .headers
                .declarations
                .anchor(declaration)
                .and_then(|anchor| anchor.owner);
        }
        let type_variables = self.active_postponed_type_variables(scope);
        let resolver = crate::symbol_resolver::SymbolResolver::new_import_scoped_with_module(
            self.table.libraries.as_ref(),
            &module,
            &imports,
        )
        .with_access_context(package, scope.source.raw(), lexical_classes)
        .with_visibility_suppression(self.scope_suppresses_visibility(scope))
        .with_type_variables(&type_variables);
        select(&resolver).ok_or_else(Self::failure)
    }

    /// The lexical visibility-suppression policy of a signature scope. It is the resolved
    /// `kotlin.Suppress` fact its owning declaration publishes — covering the file, the lexically
    /// enclosing declarations, and the declaration itself — and every signature access check reads
    /// it here: each resolver this bridge builds for the scope installs it, and a member access
    /// site reports it.
    pub(super) fn scope_suppresses_visibility(&self, scope: crate::fir::SignatureScope) -> bool {
        self.table.declaration_suppresses_visibility(scope.owner)
    }

    /// A superclass constructor call (`: pkg.Base(args)`) names its classifier apart from the
    /// supertype reference, and kotlinc reports an inaccessible one at the call's callee as well.
    /// The scope's resolver carries the declaration's lexical visibility policy, so a suppressed
    /// declaration reports neither.
    pub(super) fn check_superclass_call_access(
        &self,
        scope: crate::fir::SignatureScope,
        callee: crate::diag::Span,
        superclass: crate::types::TypeName,
    ) {
        let access = self
            .with_resolver(scope, |resolver| {
                resolver.inaccessible_classifier_access(superclass)
            })
            .ok();
        crate::trace_compiler!(
            "resolve",
            "superclass call access declaration={:?} superclass={superclass} inaccessible={access:?}",
            scope.owner,
        );
        if let Some(access) = access {
            let display = superclass.render().replace(['/', '$'], ".");
            self.record_classifier_access_diagnostic_at(
                scope.owner,
                scope.source,
                callee,
                superclass,
                super::super::inaccessible_classifier_message(&display, access),
            );
        }
    }
}
