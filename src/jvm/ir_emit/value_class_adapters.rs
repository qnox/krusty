//! The two conversions an adapter method makes between a value class's box and its carrier.
//! Bridges and function-value `invoke` methods receive boxes in erased slots and hand carriers on.

use super::*;

/// Rebuild a native scalar value class from the carrier currently on the stack. The semantic type
/// supplies both the value-class owner and its carrier; callers do not spell either JVM detail.
pub(super) fn emit_native_value_class_constructor(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    semantic: Ty,
) -> bool {
    let semantic = semantic.canonical_semantic().non_null();
    let Some(owner) = semantic.kotlin_class_internal() else {
        return false;
    };
    let Some(carrier) = semantic.scalar_value_repr() else {
        return false;
    };
    if carrier == semantic {
        return false;
    }
    let owner = owner.render();
    let descriptor = method_descriptor(&[carrier], carrier);
    let constructor = cw.methodref(&owner, "constructor-impl", &descriptor);
    let words = slot_words(carrier) as i32;
    code.invokestatic(constructor, words, words);
    true
}

/// Unbox the `value_class` on the stack to its `target` carrier with `unbox-impl`; a `nullable`
/// carrier takes a null past the instance call.
pub(super) fn emit_value_class_unbox_adapter(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    value_class: TypeName,
    target: Ty,
    nullable: bool,
) {
    let value_class = value_class.render();
    let value_class_ref = cw.class_ref(&value_class);
    code.checkcast(value_class_ref);
    let unbox = cw.methodref(
        &value_class,
        "unbox-impl",
        &format!("(){}", type_descriptor(target)),
    );
    if !nullable {
        code.invokevirtual(unbox, 0, slot_words(target) as i32);
        return;
    }
    let null = code.new_label();
    let end = code.new_label();
    code.dup();
    code.ifnull(null);
    code.invokevirtual(unbox, 0, slot_words(target) as i32);
    code.goto(end);
    code.bind(null);
    code.pop();
    code.aconst_null();
    code.bind(end);
}

/// Box a value class's `carrier` with its `box-impl`; a `nullable` carrier's null stays null.
pub(super) fn emit_value_class_box_adapter(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    value_class: TypeName,
    carrier: Ty,
    nullable: bool,
) {
    let value_class = value_class.render();
    let box_impl = cw.methodref(
        &value_class,
        "box-impl",
        &format!("({})L{value_class};", type_descriptor(carrier)),
    );
    if !nullable {
        code.invokestatic(box_impl, slot_words(carrier) as i32, 1);
        return;
    }
    let null = code.new_label();
    let end = code.new_label();
    code.dup();
    code.ifnull(null);
    code.invokestatic(box_impl, slot_words(carrier) as i32, 1);
    code.goto(end);
    code.bind(null);
    code.pop();
    code.aconst_null();
    code.bind(end);
}
