//! Override validation when a body-local classifier is checked after its plan was published.

use crate::ast::{File, FunDecl};
use crate::fir::{ActiveSourceDeclarations, ResolvedFunctionOverrideTarget, ResolvedModuleIndex};
use crate::types::TypeName;

/// Whether `method` is the implementation of an edge in `owner`'s frozen override plan.
///
/// A body group can be checked again after inherited visibility becomes final. The transient local
/// providers were consumed by the first traversal, so validation binds the active parser method to
/// its stable declaration and consumes the exact recorded edge instead of rediscovering it.
pub(crate) fn published_function_override_matches(
    index: &ResolvedModuleIndex,
    active: &ActiveSourceDeclarations,
    file: &File,
    owner: TypeName,
    method: &FunDecl,
) -> bool {
    let Some(owner) = index.classifier_declaration(owner) else {
        return false;
    };
    let callable = index
        .owned_declarations(owner)
        .iter()
        .copied()
        .find(|declaration| {
            active
                .function(file, *declaration)
                .is_some_and(|candidate| std::ptr::eq(candidate, method))
        })
        .and_then(|declaration| index.callable_for_declaration(declaration));
    callable.is_some_and(|callable| {
        index
            .function_overrides(owner)
            .iter()
            .any(|edge| edge.implementation == ResolvedFunctionOverrideTarget::Module(callable.id))
    })
}
