//! JVM spellings of the values a local or anonymous class captures.
//!
//! Common IR records a capture's source identity on its constructor parameter
//! ([`IrConstructorCapture`]); kotlinc's LocalDeclarationsLowering spells the field that stores it,
//! the constructor's `MethodParameters` entry and its `LocalVariableTable` row the same way (`$a`).
//! Every class-file surface that names a capture goes through this module.

use crate::ir::{IrClass, IrConstructorCapture};

/// The field, reflected parameter and local-variable name of a captured value.
pub(super) fn capture_name(capture: &IrConstructorCapture) -> String {
    format!("${}", capture.source_name)
}

/// The capture a class stores into `field`, if that field holds a captured value.
pub(super) fn field_capture(class: &IrClass, field: usize) -> Option<&IrConstructorCapture> {
    class
        .ctor_args
        .iter()
        .find(|argument| {
            argument
                .field_index
                .is_some_and(|index| index as usize == field)
        })
        .and_then(|argument| argument.capture.as_ref())
}
