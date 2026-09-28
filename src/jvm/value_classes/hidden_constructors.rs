//! Which constructor slots give a class kotlinc's hidden constructor.
//!
//! A class whose constructor takes a value class gets a private constructor over the carriers and a
//! public accessor taking a trailing `DefaultConstructorMarker`. kotlinc decides that from the
//! constructor's parameters when it lowers value classes: its declared parameters, an inner class's
//! outer instance, and a local class's captured values, which are already parameters by then. An
//! anonymous object's captures and a lambda class's are added after that lowering, as carriers, so
//! they never hide the constructor.

use crate::ir::IrClass;

/// Per primary-constructor slot of `class`, whether a value class there selects the hidden
/// constructor.
pub(super) fn selecting_slots(class: &IrClass) -> impl Iterator<Item = bool> + '_ {
    let captures_count = !class.is_anonymous_object && class.lambda.is_none();
    class
        .ctor_args
        .iter()
        .map(move |argument| argument.declared_ty.is_some() || captures_count)
}
