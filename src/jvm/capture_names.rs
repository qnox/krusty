//! JVM spellings of the values a local or anonymous class captures.
//!
//! Common IR records a capture's source identity on its constructor parameter
//! ([`IrConstructorCapture`]); kotlinc's LocalDeclarationsLowering spells the field that stores it,
//! the constructor's `MethodParameters` entry and its `LocalVariableTable` row the same way (`$a`).
//! Every class-file surface that names a capture goes through this module.

use crate::fir::CapturedCallableOwner;
use crate::ir::{
    FunId, IrCapturedReceiver, IrClass, IrConstructorCapture, IrFile, IrLambdaCapture,
    IrLambdaClass, IrParameterIdentity, IrParameterRole,
};
use crate::jvm::anonymous_context_labels;
use crate::jvm::suspend::cps::SuspendLambdaParameters;
use crate::types::CapturedContextKind;

/// The field a local or anonymous `class` stores `capture` in, which is also the constructor's
/// `-java-parameters` entry, and the constructor's local-variable name for it: `$a` for a value;
/// a receiver spelled as [`receiver_name`] spells it, unless the class is written in a value-class
/// member or constructor, whose static realizes that receiver as a value (`$arg0`, `$tmp0`).
pub(super) fn class_capture(
    ir: &IrFile,
    class: &IrClass,
    capture: &IrConstructorCapture,
) -> CaptureNames {
    let Some(receiver) = &capture.receiver else {
        return CaptureNames::value(&capture.source_name);
    };
    let container = class
        .enclosure
        .and_then(|enclosure| super::lifted_names::enclosure_root(ir, enclosure));
    receiver_capture(ir, std::slice::from_ref(receiver), 0, container)
}

/// JVM spelling shared by a stored receiver capture and a lifted callable's capture parameter.
/// Only nested dispatch captures need a caller-supplied occurrence ordinal.
fn receiver_name(receiver: &IrCapturedReceiver, dispatch: usize) -> String {
    match receiver {
        IrCapturedReceiver::Enclosing { .. } => format!("this${dispatch}"),
        IrCapturedReceiver::Callable { label, .. } | IrCapturedReceiver::Lambda(Some(label)) => {
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
) -> Option<CaptureNames> {
    Some(match lambda.captures.get(field)? {
        IrLambdaCapture::Value(name) => CaptureNames::value(name),
        &IrLambdaCapture::Receiver(ordinal) => {
            let container = lambda
                .lifting_root
                .as_ref()
                .and_then(|root| super::lifted_names::root_container_at(ir, root));
            receiver_capture(ir, &lambda.captured_receivers, ordinal as usize, container)
        }
    })
}

/// The field a suspend lambda's class stores `capture`, one of its constructor's recorded captures,
/// in, and the constructor parameter that passes it: spelled like [`lambda_class_capture`], from
/// the receiver origins and lifting root the class's parameter record keeps.
pub(super) fn suspend_lambda_capture(
    ir: &IrFile,
    parameters: &SuspendLambdaParameters,
    capture: &IrParameterIdentity,
) -> CaptureNames {
    match capture.role {
        IrParameterRole::CapturedValue { .. } => CaptureNames::value(
            capture
                .source_name
                .as_deref()
                .expect("a suspend lambda's captured value keeps its source name"),
        ),
        IrParameterRole::CapturedReceiver { ordinal } => {
            let container = parameters
                .lifting_root
                .as_ref()
                .and_then(|root| super::lifted_names::root_container_at(ir, root));
            receiver_capture(
                ir,
                &parameters.captured_receivers,
                ordinal as usize,
                container,
            )
        }
        role => panic!("a suspend lambda's constructor captures no {role:?}"),
    }
}

/// The field a class stores a capture in and the constructor parameter that passes it.
pub(super) struct CaptureNames {
    pub(super) field: String,
    pub(super) parameter: String,
}

impl CaptureNames {
    /// A captured value: `$` before its source name, for both.
    fn value(source_name: &str) -> Self {
        let name = format!("${source_name}");
        Self {
            parameter: name.clone(),
            field: name,
        }
    }
}

/// Receiver `ordinal` of `receivers` stored by a class written in `container`: named as
/// [`realized_receiver`] names it, and passed as `$receiver` where kotlinc's receiver convention
/// applies.
fn receiver_capture(
    ir: &IrFile,
    receivers: &[IrCapturedReceiver],
    ordinal: usize,
    container: Option<FunId>,
) -> CaptureNames {
    let (field, receiver) = realized_receiver(ir, receivers, ordinal, container);
    CaptureNames {
        parameter: if receiver {
            "$receiver".to_string()
        } else {
            field.clone()
        },
        field,
    }
}

/// Receiver `ordinal` of `receivers`, captured by a callable lifted out of `container`.
///
/// kotlinc lowers a value class's members and constructors to statics before it lifts what they
/// declare, so the receivers of such a declaration are no receivers any more but parameters of the
/// static, which a class captures like any value, under its field name. The enclosing instance is
/// named after the value the static realizes it as, `$`-prefixed: a member's or accessor's carrier
/// parameter (`$arg0`), or the temporary a `constructor-impl` holds it in. That holds through any
/// local or anonymous class between: the static's value is captured into each class in turn, so
/// the instance of the value class is a value wherever it is captured. The declaration's own
/// extension receiver keeps its `$this_name`. A local function is lifted, not lowered to a
/// static, so its extension receiver stays one.
fn realized_receiver(
    ir: &IrFile,
    receivers: &[IrCapturedReceiver],
    ordinal: usize,
    container: Option<FunId>,
) -> (String, bool) {
    let receiver = &receivers[ordinal];
    if let Some(value) = realized_instance(ir, receiver, container) {
        return (format!("${value}"), false);
    }
    // Only the enclosing instances still captured as instances number kotlinc's `this$N`.
    let dispatch = receivers[..ordinal]
        .iter()
        .filter(|receiver| matches!(receiver, IrCapturedReceiver::Enclosing { .. }))
        .filter(|receiver| realized_instance(ir, receiver, container).is_none())
        .count();
    let name = receiver_name(receiver, dispatch);
    match receiver {
        IrCapturedReceiver::Callable {
            owner: CapturedCallableOwner::Declaration,
            ..
        } if container
            .is_some_and(|container| value_class_receiver_value(ir, container).is_some()) =>
        {
            (name, false)
        }
        _ => (name, uses_receiver_constructor_parameter(receiver)),
    }
}

/// The value a value-class static realizes the enclosing instance `receiver` as, when a callable
/// or class lifted out of `container` captures it: `container` itself, or the member of a local
/// or anonymous class further out, belongs to the instance's class and is such a static.
fn realized_instance(
    ir: &IrFile,
    receiver: &IrCapturedReceiver,
    container: Option<FunId>,
) -> Option<String> {
    let IrCapturedReceiver::Enclosing { classifier } = receiver else {
        return None;
    };
    let member = super::lifted_names::classifier_member(ir, container?, *classifier)?;
    value_class_receiver_value(ir, member)
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

/// Whether kotlinc calls the capture's constructor parameter `$receiver` instead of giving it the
/// same spelling as its field.
fn uses_receiver_constructor_parameter(receiver: &IrCapturedReceiver) -> bool {
    matches!(
        receiver,
        IrCapturedReceiver::Enclosing { .. } | IrCapturedReceiver::Callable { .. }
    )
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
