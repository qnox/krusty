//! JVM storage of a class's compiler-supplied constructor prefix.

use crate::ir::IrClass;

pub(super) mod captured_assignment;

/// How many leading fields store the constructor prefix. A local class keeps a capture that only
/// its constructor code reads in the parameter, so it can have fewer prefix fields than prefix
/// parameters; so can an anonymous object, whose forwarded super-constructor arguments are
/// parameters only.
pub(super) fn constructor_prefix_fields(class: &IrClass) -> usize {
    let prefix = usize::try_from(class.constructor_prefix_count)
        .expect("constructor prefix count does not fit usize");
    class.ctor_args.get(..prefix).map_or(prefix, |prefix| {
        prefix.iter().filter(|argument| argument.is_field).count()
    })
}

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
