//! Overrides between a Kotlin extension and a declaration of a language without extension
//! receivers.
//!
//! Java has no extension receivers, so kotlinc's Java override checker matches a Kotlin
//! extension's receiver against a Java method's first parameter: `void foo(T t)` in a Java
//! interface overrides `fun B.foo()` of its Kotlin supertype `Base<T>`, and a Kotlin class then
//! overrides both with one `fun T.foo()`. A Kotlin declaration never overrides a Java method this
//! way: `override fun T.foo()` directly under the Java interface overrides nothing.

use super::{
    function_override_input_shape, override_input_shapes_match, resolution_subtype,
    OverrideInputShape,
};
use crate::libraries::{FnKind, FunctionInfo};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// Whether `function` overrides a Kotlin extension through its first parameter, as its provider
/// published ([`FunctionInfo::receiver_as_first_parameter`]).
fn declares_receiver_as_parameter(function: &FunctionInfo) -> bool {
    function.receiver_as_first_parameter
        && function.kind == FnKind::Member
        && function.context_count == 0
}

/// Whether the Java member `implementation` overrides the Kotlin extension `inherited` that its
/// own classifier inherits: the extension's receiver is compared as the Java method's first
/// parameter. Only the Java declaration's own checker matches this way; a Kotlin extension never
/// overrides a Java method.
pub(super) fn function_receiver_parameter_slots_match(
    source: &dyn SymbolSource,
    inherited: &FunctionInfo,
    implementation: &FunctionInfo,
) -> bool {
    if inherited.kind != FnKind::Extension
        || inherited.context_count != 0
        || !declares_receiver_as_parameter(implementation)
        || implementation.callable.owner == inherited.callable.owner
        || !resolution_subtype(
            source,
            Ty::obj_name(implementation.callable.owner),
            Ty::obj_name(inherited.callable.owner),
        )
    {
        return false;
    }
    let Some(receiver) = inherited.semantic_receiver() else {
        return false;
    };
    let mut inherited_params = vec![receiver];
    inherited_params.extend(inherited.semantic_params().iter().copied());
    let implementation_params = implementation.semantic_params();
    override_input_shapes_match(
        source,
        OverrideInputShape {
            params: &inherited_params,
            receiver: None,
            ..function_override_input_shape(inherited, &[])
        },
        function_override_input_shape(implementation, &implementation_params),
    )
}
