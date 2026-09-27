//! JVM spellings of the values a local or anonymous class captures.
//!
//! Common IR records a capture's source identity on its constructor parameter
//! ([`IrConstructorCapture`]); kotlinc's LocalDeclarationsLowering spells the field that stores it,
//! the constructor's `MethodParameters` entry and its `LocalVariableTable` row the same way (`$a`).
//! Every class-file surface that names a capture goes through this module.

use crate::ir::{IrCapturedReceiver, IrClass, IrConstructorCapture};

/// The field and reflected parameter name of a capture: `$a` for a value, `this$0` for the
/// enclosing instance, and `$this_<label>` for a callable's or lambda's receiver (`$this` for an
/// unlabeled lambda's).
pub(super) fn capture_name(capture: &IrConstructorCapture) -> String {
    match &capture.receiver {
        None => format!("${}", capture.source_name),
        Some(IrCapturedReceiver::Enclosing) => "this$0".to_string(),
        Some(IrCapturedReceiver::Callable(label) | IrCapturedReceiver::Lambda(Some(label))) => {
            format!("$this_{label}")
        }
        Some(IrCapturedReceiver::Lambda(None)) => "$this".to_string(),
    }
}

/// The constructor's local-variable name for a capture. kotlinc calls the enclosing instance and a
/// named callable's receiver `$receiver` there; everything else is named like its field.
pub(super) fn capture_parameter_local(capture: &IrConstructorCapture) -> String {
    match capture.receiver {
        Some(IrCapturedReceiver::Enclosing | IrCapturedReceiver::Callable(_)) => {
            "$receiver".to_string()
        }
        _ => capture_name(capture),
    }
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
