//! JVM spellings of the values a local or anonymous class captures.
//!
//! Common IR records a capture's source identity on its constructor parameter
//! ([`IrConstructorCapture`]); kotlinc's LocalDeclarationsLowering spells the field that stores it,
//! the constructor's `MethodParameters` entry and its `LocalVariableTable` row the same way (`$a`).
//! Every class-file surface that names a capture goes through this module.

use crate::ir::{
    FunId, IrCapturedReceiver, IrClass, IrConstructorCapture, IrFile, IrLambdaCapture,
    IrLambdaClass,
};
use crate::jvm::anonymous_context_labels;
use crate::types::CapturedContextKind;

/// The field and reflected parameter name of a capture: `$a` for a value, `this$0` for the
/// enclosing instance, `$this_<label>` for a callable's or lambda's receiver (`$this` for an
/// unlabeled lambda's), `$` before the parameter's own label for an anonymous or function-type
/// context parameter (`$$context-Box`), and `$` before a legacy context receiver's parameter name
/// (`$$context_receiver_0`).
pub(super) fn capture_name(capture: &IrConstructorCapture) -> String {
    match &capture.receiver {
        None => format!("${}", capture.source_name),
        Some(receiver) => receiver_name(receiver, 0),
    }
}

/// JVM spelling shared by a stored receiver capture and a lifted callable's capture parameter.
/// Only nested dispatch captures need a caller-supplied occurrence ordinal.
pub(super) fn receiver_name(receiver: &IrCapturedReceiver, dispatch: usize) -> String {
    match receiver {
        IrCapturedReceiver::Enclosing => format!("this${dispatch}"),
        IrCapturedReceiver::Callable(label) | IrCapturedReceiver::Lambda(Some(label)) => {
            format!("$this_{label}")
        }
        IrCapturedReceiver::Lambda(None) => "$this".to_string(),
        IrCapturedReceiver::Context { kind, types, index } => match kind {
            CapturedContextKind::Anonymous | CapturedContextKind::FunctionType => {
                format!(
                    "${}",
                    anonymous_context_labels::label_at(types, *index as usize)
                )
            }
            CapturedContextKind::LegacyReceiver => format!("$$context_receiver_{index}"),
        },
    }
}

/// A lifted callable's captured implicit receiver as kotlinc spells it: the parameter's name (also a
/// lambda class's field), and whether a lambda class's constructor takes it as `$receiver`; `None`
/// when `function` publishes no origin for that receiver.
pub(super) fn lifted_receiver(
    ir: &IrFile,
    function: FunId,
    ordinal: usize,
) -> Option<(String, bool)> {
    let receivers = &ir.fn_params.get(&function)?.captured_receivers;
    receivers.get(ordinal)?;
    let container = super::lifted_names::root_container(ir, function);
    Some(realized_receiver(ir, receivers, ordinal, container))
}

/// The field a lambda class stores its capture `field` in, and the constructor parameter that
/// passes it: `$a` for a captured value; a captured receiver spelled as [`lifted_receiver`] spells
/// it, from the lambda's own record of the receivers and of where it was lifted from.
pub(super) fn lambda_class_capture(
    ir: &IrFile,
    lambda: &IrLambdaClass,
    field: usize,
) -> Option<LambdaCaptureNames> {
    Some(match lambda.captures.get(field)? {
        IrLambdaCapture::Value(name) => {
            let name = format!("${name}");
            LambdaCaptureNames {
                parameter: name.clone(),
                field: name,
            }
        }
        &IrLambdaCapture::Receiver(ordinal) => {
            let container = lambda
                .lifting_root
                .as_ref()
                .and_then(|root| super::lifted_names::root_container_at(ir, root));
            let (field, receiver) =
                realized_receiver(ir, &lambda.captured_receivers, ordinal as usize, container);
            LambdaCaptureNames {
                parameter: if receiver {
                    "$receiver".to_string()
                } else {
                    field.clone()
                },
                field,
            }
        }
    })
}

/// A lambda class capture's field name and its constructor parameter's.
pub(super) struct LambdaCaptureNames {
    pub(super) field: String,
    pub(super) parameter: String,
}

/// Receiver `ordinal` of `receivers`, captured by a callable lifted out of `container`.
///
/// kotlinc lowers a value class's members and constructors to statics before it lifts what they
/// declare, so the enclosing instance such a callable captures is no dispatch receiver but the
/// value the static realizes it as, `$`-prefixed like any captured value: a member's or accessor's
/// carrier parameter (`$arg0`), or the temporary a `constructor-impl` holds it in.
fn realized_receiver(
    ir: &IrFile,
    receivers: &[IrCapturedReceiver],
    ordinal: usize,
    container: Option<FunId>,
) -> (String, bool) {
    let receiver = &receivers[ordinal];
    if *receiver == IrCapturedReceiver::Enclosing {
        if let Some(value) =
            container.and_then(|container| value_class_receiver_value(ir, container))
        {
            return (format!("${value}"), false);
        }
    }
    (
        lifted_receiver_name(receivers, ordinal),
        uses_receiver_constructor_parameter(receiver),
    )
}

/// The name of the value the value-class static `container` realizes its receiver as. kotlinc's
/// `JvmInlineClassLowering` passes a member's receiver as the carrier parameter it names `arg0`,
/// and holds a constructor's in the `constructor-impl`'s first temporary: `tmp0` for the primary
/// constructor running the `init` blocks, `tmp0_$this` after the name hint a secondary
/// constructor gives it.
fn value_class_receiver_value(ir: &IrFile, container: FunId) -> Option<String> {
    if ir.jvm_value_class_receiver_impls.contains(&container) {
        let carrier = ir
            .function_parameter_identities(container)
            .and_then(<[_]>::first)
            .expect("a value-class receiver implementation leads with its carrier");
        return Some(
            super::parameter_names::local_variable(carrier, "")
                .expect("a value-class carrier parameter is named"),
        );
    }
    match ir.jvm_value_class_constructor_impls.get(&container)? {
        0 => Some("tmp0".to_string()),
        _ => Some("tmp0_$this".to_string()),
    }
}

/// kotlinc's `LocalDeclarationsLowering` name for the parameter a captured implicit receiver is
/// lifted into. The ordinal selects the semantic receiver origin; only preceding dispatch
/// receivers contribute to kotlinc's `this$N` occurrence number.
fn lifted_receiver_name(receivers: &[IrCapturedReceiver], ordinal: usize) -> String {
    let receiver = receivers
        .get(ordinal)
        .expect("a lifted callable publishes the origin of each captured receiver");
    let dispatch = receivers[..ordinal]
        .iter()
        .filter(|receiver| matches!(receiver, IrCapturedReceiver::Enclosing))
        .count();
    receiver_name(receiver, dispatch)
}

/// Whether kotlinc calls the capture's constructor parameter `$receiver` instead of giving it the
/// same spelling as its field.
pub(super) fn uses_receiver_constructor_parameter(receiver: &IrCapturedReceiver) -> bool {
    matches!(
        receiver,
        IrCapturedReceiver::Enclosing | IrCapturedReceiver::Callable(_)
    )
}

/// The constructor's local-variable name for a capture. kotlinc calls the enclosing instance and a
/// named callable's receiver `$receiver` there; everything else is named like its field.
pub(super) fn capture_parameter_local(capture: &IrConstructorCapture) -> String {
    match &capture.receiver {
        Some(receiver) if uses_receiver_constructor_parameter(receiver) => "$receiver".to_string(),
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
