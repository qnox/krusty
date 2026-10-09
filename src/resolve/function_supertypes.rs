//! `FunctionalTypeWithExtensionAsSupertype`: without it, a classifier may not list a function type
//! with an extension receiver or context parameters among its supertypes, written directly or
//! through a type alias. kotlinc reports each such supertype reference.

use super::*;

impl Checker<'_> {
    /// Report each supertype of `class` that expands to an extension or contextual function type
    /// while the feature is off. Its type parameters are the classifier's, in `class_tparams`.
    pub(super) fn check_function_supertypes(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
        class_tparams: &TParams,
    ) {
        if self
            .file
            .language_gates
            .functional_type_with_extension_as_supertype
            .is_enabled()
        {
            return;
        }
        let header_scope = scope.child(ScopeKind::Block);
        header_scope.declare_tparams(&class.type_params, class_tparams, |_| false);
        for reference in &class.supertypes {
            // The supertype was resolved, and any failure reported, with the classifier's header.
            let diagnostics = self.diags.diags.len();
            let supertype = self.type_ref_ty(&header_scope, reference);
            self.diags.diags.truncate(diagnostics);
            if let Ty::Fun(signature) = supertype {
                if signature.has_receiver || signature.context_count > 0 {
                    self.diags.error(
                        reference.span,
                        "extension or contextual function type is not allowed as a supertype.",
                    );
                }
            }
        }
    }
}
