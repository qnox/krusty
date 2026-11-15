//! Declarations selected for suspend lowering's coroutine runtime operations.

use crate::ir::IrCoroutineRuntime;
use crate::libraries::{FnKind, PropKind};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::type_name;

/// The dependency declarations coroutine lowering may need for this compilation.
pub(super) fn selected(source: &dyn SymbolSource) -> Option<IrCoroutineRuntime> {
    let intrinsics = source.symbols(
        SymbolNamespace::Package(type_name("kotlin/coroutines/intrinsics")),
        "COROUTINE_SUSPENDED",
    );
    let mut suspended = intrinsics
        .callables
        .properties()
        .iter()
        .filter(|property| property.kind == PropKind::TopLevel && property.receiver.is_none());
    let (Some(suspended), None) = (suspended.next(), suspended.next()) else {
        return None;
    };

    let kotlin = source.symbols(SymbolNamespace::Package(type_name("kotlin")), "getOrThrow");
    let result = type_name("kotlin/Result");
    let mut get_or_throw = kotlin.callables.functions().iter().filter(|function| {
        function.kind == FnKind::Extension
            && function
                .receiver
                .and_then(|receiver| receiver.non_null().obj_internal())
                == Some(result)
    });
    let (Some(get_or_throw), None) = (get_or_throw.next(), get_or_throw.next()) else {
        return None;
    };
    Some(IrCoroutineRuntime {
        suspended: suspended.getter.external_property_identity?,
        get_or_throw: get_or_throw.callable.external_identity?,
    })
}
