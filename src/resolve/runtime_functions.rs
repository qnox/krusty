//! Stdlib functions kotlinc's own lowering calls (a counted loop's `getProgressionLastElement`, an
//! exhaustive `when`'s `throwNoWhenBranchMatchedException`), selected from the provider's
//! declarations as a source call to them would be.

use crate::fir::ExternalCallableId;
use crate::types::Ty;

use super::Checker;

/// A stdlib function a compiler-inserted call reaches, selected from the provider's declarations.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeFunction {
    pub function: ExternalCallableId,
    pub parameters: Box<[Ty]>,
    pub result: Ty,
}

impl Checker<'_> {
    /// Selects the stdlib function `name` of `package` a compiler-inserted call with `arguments`
    /// reaches, through the same candidate collection and overload selection as a source call to
    /// it. The call is kotlinc's own, so the declaration's visibility (`@PublishedApi internal`)
    /// does not restrict it; an ambiguous or inapplicable family selects nothing.
    pub(super) fn select_runtime_function(
        &self,
        package: crate::types::TypeName,
        name: &str,
        arguments: &[Ty],
    ) -> Option<RuntimeFunction> {
        let scope = [package];
        let resolver = self.resolver_in_scope(&scope);
        let arguments = arguments
            .iter()
            .map(|argument| crate::symbol_resolver::CallArgKind::Typed(*argument))
            .collect::<Vec<_>>();
        let (_, callable) = resolver.select_top_level_function_candidates_ignoring_visibility(
            name,
            resolver.top_level_candidates(name),
            &arguments,
            &[],
        )?;
        // The selection's applied callable carries the declaration's Kotlin result (`Nothing`),
        // not the platform erasure (`Void`) the provider's raw candidate record keeps.
        Some(RuntimeFunction {
            function: callable.external_identity?,
            parameters: callable.params.into_boxed_slice(),
            result: callable.ret,
        })
    }
}
