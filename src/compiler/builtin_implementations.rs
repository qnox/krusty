//! Builtin implementation facts for checked source IR and generated KLIB support bodies.
//!
//! A backend that carries a builtin value in a representation of its own (a string, a box)
//! dispatches a call of `CharSequence.length` or `Number.toLong` on such a value to the class's
//! own override. Which override that is comes from the member hierarchy's override edges, which
//! the frontend owns; the handoff freezes the answer into each member's
//! [`crate::backend::BackendCallableFact`] so no backend reads the hierarchy itself.

use crate::backend::{BuiltinImplementation, BuiltinOverrideDeclaration, CheckedBackendCallables};
use crate::symbol_resolver::{builtin_overrides, BuiltinOverride};
use crate::symbol_source::SymbolSource;

/// Record the implementation facts produced by the checker and carried by common IR.
pub(super) fn record_checked(
    callables: &mut CheckedBackendCallables,
    ir: &crate::ir::IrFile,
) -> Result<(), crate::fir::ExternalCallableId> {
    callables.record_checked_builtin_implementations(&ir.external_builtin_implementations)
}

/// Record builtin implementations for generated KLIB support calls that have no source checker
/// result to carry through IR yet.
pub(super) fn record_dependency_support(
    callables: &mut CheckedBackendCallables,
    source: &dyn SymbolSource,
) {
    callables.record_builtin_implementations(|owner, name, identity| {
        builtin_overrides(source, owner, name, identity)
            .into_iter()
            .map(implementation)
            .collect()
    });
}

/// The handoff form of one builtin class's override: the operation its provider realizes it as,
/// and its declared parameters and result.
fn implementation(implementation: BuiltinOverride) -> BuiltinImplementation {
    let declaration = implementation
        .declaration
        .map(|declaration| BuiltinOverrideDeclaration {
            operation: declaration
                .callable
                .compiler_intrinsic
                .or(declaration.callable.member_realization.compiler_intrinsic()),
            params: declaration.identity.params,
            ret: declaration.identity.ret,
        });
    BuiltinImplementation {
        receiver: implementation.receiver,
        declaration,
    }
}
