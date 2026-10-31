//! Scope-tower position of a bare `Name(args)` constructor against same-named receiver-less
//! functions. A classifier found on a file/import rung contributes its constructors on that rung
//! only, so an applicable function on a nearer rung takes the call first.

use super::*;

impl Checker<'_> {
    /// Whether a receiver-less function on a rung nearer than `classifier`'s own takes the bare
    /// call `name(args)`. kotlinc 2.4.21 binds `Pair(1, 2)` to a same-package
    /// `fun Pair(a: Int, b: Int)` (or a star-imported one) ahead of the default-imported
    /// `kotlin.Pair` constructor, and still constructs the pair when that function is inapplicable.
    pub(super) fn nearer_function_takes_bare_constructor_call(
        &self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        (name, classifier): (&str, TypeName),
        explicit_type_args: &[Ty],
        expected: Option<Ty>,
    ) -> bool {
        let resolver = self.resolver();
        if !resolver.top_level_function_precedes_classifier(name, classifier) {
            return false;
        }
        let candidates = resolver
            .top_level_candidates(name)
            .into_iter()
            .filter(|candidate| candidate.kind == crate::libraries::FnKind::TopLevel)
            .collect::<Vec<_>>();
        matches!(
            self.select_callable_candidate(
                scope,
                call_args,
                explicit_type_args,
                None,
                CallResultConstraint::direct(expected),
                candidates,
            ),
            Some(
                CallableCandidateSelection::Selected(_) | CallableCandidateSelection::Ambiguous(_)
            )
        )
    }
}
