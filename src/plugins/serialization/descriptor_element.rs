//! Frontend planning and common-IR realization for
//! `ClassSerialDescriptorBuilder.element<T>(...)`.

use crate::ir::{Callee, IrConst, IrExpr, IrFile};
use crate::libraries::InlineKind;
use crate::plugins::{FrontendSelectedCall, PluginContext, PluginExpressionPlan};
use crate::types::{type_name, Ty};

use super::{element_serializer_expr, KSERIALIZER_FQ};

pub(super) const SERIAL_DESCRIPTORS_FQ: &str =
    "kotlinx/serialization/descriptors/SerialDescriptorsKt";
pub(super) const CLASS_SERIAL_DESCRIPTOR_BUILDER_FQ: &str =
    "kotlinx/serialization/descriptors/ClassSerialDescriptorBuilder";
const BUILDER_ELEMENT_DESC: &str = "(Ljava/lang/String;\
Lkotlinx/serialization/descriptors/SerialDescriptor;Ljava/util/List;Z)V";

/// Attach the plugin operation to the exact descriptor helper selected and argument-mapped by the
/// frontend. The body phase must not rediscover it from a lowered JVM owner, `$default` mask, or
/// source spelling.
pub(super) fn plan(call: &FrontendSelectedCall) -> Option<PluginExpressionPlan> {
    let signature = call.generic_sig.as_ref()?;
    let builder = type_name(CLASS_SERIAL_DESCRIPTOR_BUILDER_FQ);
    if call.owner != type_name(SERIAL_DESCRIPTORS_FQ)
        || !matches!(call.name.as_str(), "element" | "element$default")
        || !call.inline.can_inline()
        || signature.formals.len() != 1
        || signature.receiver.and_then(Ty::kotlin_class_internal) != Some(builder)
        || call.params.len() != 3
    {
        return None;
    }
    let (implicit_receiver, mut operands) = match call.explicit_receiver {
        Some((receiver, receiver_ty)) if receiver_ty.kotlin_class_internal() == Some(builder) => {
            (false, vec![(receiver, receiver_ty)])
        }
        None if call.implicit_receiver.and_then(Ty::kotlin_class_internal) == Some(builder) => {
            (true, Vec::new())
        }
        None | Some(_) => return None,
    };
    let [Some(name), annotations, is_optional] = call.argument_slots.as_slice() else {
        return None;
    };
    let [Some(element)] = call.type_arguments.as_slice() else {
        return None;
    };
    let operation = match (annotations.is_none(), is_optional.is_none()) {
        (false, false) => "descriptorElement",
        (true, false) => "descriptorElementDefaultAnnotations",
        (false, true) => "descriptorElementDefaultOptional",
        (true, true) => "descriptorElementDefaultBoth",
    };
    operands.push((*name, call.params[0]));
    if let Some(annotations) = annotations {
        operands.push((*annotations, call.params[1]));
    }
    if let Some(is_optional) = is_optional {
        operands.push((*is_optional, call.params[2]));
    }
    Some(PluginExpressionPlan {
        plugin: "serialization",
        operation,
        data: Vec::new(),
        types: vec![*element],
        implicit_receiver,
        operands,
    })
}

/// Realize one planned descriptor-element operation. `true` means the kind belongs to this module;
/// a malformed or unsupported plan deliberately remains a placeholder so emission fails closed.
pub(super) fn specialize(
    ir: &mut IrFile,
    ctx: &PluginContext,
    index: usize,
    kind: &str,
    exprs: &[u32],
    types: &[Ty],
) -> bool {
    let defaults = match kind {
        "descriptorElement" => (false, false),
        "descriptorElementDefaultAnnotations" => (true, false),
        "descriptorElementDefaultOptional" => (false, true),
        "descriptorElementDefaultBoth" => (true, true),
        _ => return false,
    };
    let [element] = types else {
        return true;
    };
    let Some((&receiver, rest)) = exprs.split_first() else {
        return true;
    };
    let Some((&name, supplied)) = rest.split_first() else {
        return true;
    };
    let (annotations, is_optional) = match (defaults.0, defaults.1, supplied) {
        (false, false, [annotations, is_optional]) => (Some(*annotations), Some(*is_optional)),
        (true, false, [is_optional]) => (None, Some(*is_optional)),
        (false, true, [annotations]) => (Some(*annotations), None),
        (true, true, []) => (None, None),
        _ => return true,
    };
    let Some(serializer) = element_serializer_expr(ir, ctx, element) else {
        return true;
    };
    let descriptor = ir.add_expr(IrExpr::Call {
        callee: Callee::Virtual {
            owner: type_name(KSERIALIZER_FQ),
            name: "getDescriptor".to_string(),
            descriptor: "()Lkotlinx/serialization/descriptors/SerialDescriptor;".to_string(),
            params: None,
            interface: true,
        },
        dispatch_receiver: Some(serializer),
        args: vec![],
    });
    let annotations = annotations.unwrap_or_else(|| {
        ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: type_name("kotlin/collections/CollectionsKt"),
                name: "emptyList".to_string(),
                descriptor: "()Ljava/util/List;".to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![],
        })
    });
    let is_optional =
        is_optional.unwrap_or_else(|| ir.add_expr(IrExpr::Const(IrConst::Boolean(false))));
    ir.exprs[index] = IrExpr::Call {
        callee: Callee::Virtual {
            owner: type_name(CLASS_SERIAL_DESCRIPTOR_BUILDER_FQ),
            name: "element".to_string(),
            descriptor: BUILDER_ELEMENT_DESC.to_string(),
            params: None,
            interface: false,
        },
        dispatch_receiver: Some(receiver),
        args: vec![name, descriptor, annotations, is_optional],
    };
    true
}
