//! JVM storage of a class's compiler-supplied constructor prefix.

use crate::ir::IrClass;

/// Whether instance field `field_index` stores a constructor-prefix value: the enclosing instance
/// of an inner class, or a value a local class or anonymous object captures.
///
/// kotlinc realizes that storage as package-visible `final synthetic` (`JvmVisibilityPolicy`
/// `forCapturedField`), so a class nested in the capturing class reads it directly. A private field
/// would need a synthetic accessor and fail to link without one.
pub(super) fn stores_constructor_prefix(class: &IrClass, field_index: usize) -> bool {
    class.ctor_args[..class.constructor_prefix_count as usize]
        .iter()
        .any(|argument| {
            argument.is_field && argument.field_index == u32::try_from(field_index).ok()
        })
}
