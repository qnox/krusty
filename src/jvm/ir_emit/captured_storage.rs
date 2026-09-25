//! JVM storage of a class's compiler-supplied constructor prefix.

use crate::ir::IrClass;

/// Whether instance field `field_index` stores a constructor-prefix value: the enclosing instance
/// of an inner class, or a value a local class or anonymous object captures.
///
/// kotlinc realizes that storage as package-visible `final synthetic` (`JvmVisibilityPolicy`
/// `forCapturedField`), so a class nested in the capturing class reads it directly. A private field
/// would need a synthetic accessor and fail to link without one.
pub(super) fn stores_constructor_prefix(class: &IrClass, field_index: usize) -> bool {
    let field_index = u32::try_from(field_index).expect("too many JVM fields");
    let prefix_count = usize::try_from(class.constructor_prefix_count)
        .expect("constructor prefix count does not fit usize");
    class
        .ctor_args
        .get(..prefix_count)
        .expect("constructor prefix exceeds its argument layout")
        .iter()
        .any(|argument| argument.is_field && argument.field_index == Some(field_index))
}
