//! The block of `kotlin.coroutines.suspendCoroutine`, which the JVM inlines in place.
//!
//! `suspendCoroutine` is an ordinary inline call, and the checker and common lowering treat it as
//! one. Its block parameter is `crossinline` only to forbid non-local returns: kotlinc's JVM
//! inliner inlines that block in place (`isCoroutineIntrinsic` in `IrInlineScopeResolver`), so its
//! code stays the calling class's own instead of being compiled as code that may move into another
//! class. That is a JVM inlining decision. It is made where dependency calls are realized, from the
//! selected declaration's identity, and published with the operands' inline roles, which every
//! later copy or relocation of the call carries.

use krusty::backend::BackendCallableFact;
use krusty::types::{type_name, InlineParameterModifier, SemanticCallableOwner};

/// The inline roles the JVM inliner gives the operands of a call to `callable`, whose declaration
/// publishes `declared`: the declared roles, except that the block of the builtin
/// `suspendCoroutine` is inlined in place like an ordinary inline lambda.
pub(super) fn jvm_inline_roles(
    callable: &BackendCallableFact,
    declared: Box<[InlineParameterModifier]>,
) -> Box<[InlineParameterModifier]> {
    if !is_builtin_suspend_coroutine(callable) {
        return declared;
    }
    declared
        .iter()
        .map(|&modifier| match modifier {
            InlineParameterModifier::Crossinline => InlineParameterModifier::None,
            other => other,
        })
        .collect()
}

/// Whether `callable` is the declaration `kotlin.coroutines.suspendCoroutine`: a top-level
/// function of that package with that name, as kotlinc's `isBuiltInSuspendCoroutine` compares
/// the callable's qualified name.
fn is_builtin_suspend_coroutine(callable: &BackendCallableFact) -> bool {
    callable.declaration_owner
        == Some(SemanticCallableOwner::Package(type_name(
            "kotlin/coroutines",
        )))
        && callable.name == "suspendCoroutine"
}
