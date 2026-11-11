//! What each builtin primitive runs for a library classifier's member, answered at the backend
//! handoff from the member hierarchy.
//!
//! A backend that carries a primitive as an object (a box) dispatches a call of `Number.toLong`
//! on such a value to the primitive's own override. Which override that is comes from the member
//! hierarchy's override edges, which the frontend owns; the handoff freezes the answer into each
//! member's [`crate::backend::BackendCallableFact`] so no backend reads the hierarchy itself.

use crate::backend::{
    CheckedBackendCallables, PrimitiveImplementation, PrimitiveOverrideDeclaration,
};
use crate::symbol_resolver::{
    function_declaration_identity, primitive_overrides, PrimitiveOverride,
};
use crate::symbol_source::SymbolSource;

/// Record every called library member's primitive implementations, as `source` declares them.
pub(super) fn record(callables: &mut CheckedBackendCallables, source: &dyn SymbolSource) {
    callables.record_primitive_implementations(|owner, name, identity| {
        primitive_overrides(source, owner, name, identity)
            .into_iter()
            .map(implementation)
            .collect()
    });
}

/// The handoff form of one primitive's override: the operation its provider realizes it as, and
/// its declared parameters and result.
fn implementation(implementation: PrimitiveOverride) -> PrimitiveImplementation {
    let declaration = implementation.declaration.map(|function| {
        let identity = function_declaration_identity(&function);
        PrimitiveOverrideDeclaration {
            operation: function
                .callable
                .compiler_intrinsic
                .or(function.callable.member_realization.compiler_intrinsic()),
            params: identity.params,
            ret: identity.ret,
        }
    });
    PrimitiveImplementation {
        receiver: implementation.receiver,
        declaration,
    }
}
