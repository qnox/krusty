//! Selects the stdlib declarations a suspend function's lowering calls (see
//! [`crate::ir::IrCoroutineRuntime`]) among the declarations the compilation sees.

use crate::ir::IrCoroutineRuntime;
use crate::libraries::{FnKind, PropKind};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::type_name;

/// The runtime of a file that declares a suspend function, or `None` when the classpath declares
/// no single `COROUTINE_SUSPENDED` property or no single `Result.getOrThrow` extension.
pub(super) fn select(source: &dyn SymbolSource) -> Option<IrCoroutineRuntime> {
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
