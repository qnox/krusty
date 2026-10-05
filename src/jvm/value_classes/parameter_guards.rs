//! The parameter entry guards that survive value-class realization.
//!
//! kotlinc's `ExpressionCodegen.generateNonNullAssertions` guards a parameter of a function it does
//! not consider private or synthetic when the parameter's type, with each value class unwrapped to
//! its carrier (`unboxInlineClass`), is non-null and its JVM type is not primitive.
//! [`crate::jvm::parameter_assertions`] records the declared contracts over semantic types. Once
//! each value class has a carrier, this module keeps the contracts that rule still admits, and
//! gives the static members this pass synthesizes (`constructor-impl`, the `-impl` statics) the
//! contracts of the declarations they realize.

use super::{erase, is_ref, vc_underlying_nullable, Under};
use crate::ir::{IrFunction, IrParameterCheck, IrSecondaryCtor};
use crate::types::Ty;

/// Whether a parameter declared as `declared` keeps its non-null contract on the JVM: it is still a
/// reference, and no value class it unwraps admits a null carrier (`X(val v: String?)`).
pub(super) fn carrier_keeps_guard(declared: &Ty, under: &Under) -> bool {
    is_ref(&erase(declared, under)) && !vc_underlying_nullable(declared, under)
}

/// A value-class member realized as a static over its carrier takes that carrier as parameter 0.
/// The box already holds it, so kotlinc never guards it; the declared parameters keep their own
/// contracts one position later.
pub(super) fn prepend_carrier(function: &mut IrFunction, carrier: Ty) {
    function.params.insert(0, carrier);
    if !function.param_checks.is_empty() {
        function.param_checks.insert(0, None);
    }
}

/// The contracts of a value class's static `constructor-impl`, which kotlinc compiles from the
/// constructor declaration it replaces: one per declared parameter. The signature pass then drops
/// the ones whose carrier is primitive or admits null, as for every other function.
pub(super) fn constructor_impl_contracts(
    declared: impl IntoIterator<Item = bool>,
) -> Vec<Option<IrParameterCheck>> {
    declared
        .into_iter()
        .map(|guarded| guarded.then_some(IrParameterCheck::NonNull))
        .collect()
}

/// A regular class's secondary constructor taking a value class is private in the class file and
/// reached through a public `DefaultConstructorMarker` overload. kotlinc decides the guards from
/// the declaration, which keeps its visibility, so the private constructor guards each parameter
/// whose value-class carrier keeps the contract; the marker overload guards none.
pub(super) fn retain_secondary_constructor_guards(
    constructor: &mut IrSecondaryCtor,
    under: &Under,
) {
    for (declared, check) in constructor
        .params
        .iter()
        .zip(constructor.param_checks.iter_mut())
    {
        let value_class = declared
            .non_null()
            .obj_internal()
            .is_some_and(|name| under.contains_key(&name));
        if value_class && !carrier_keeps_guard(declared, under) {
            *check = None;
        }
    }
}
