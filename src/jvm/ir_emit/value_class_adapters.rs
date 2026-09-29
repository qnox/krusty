//! The two conversions an adapter method makes between a value class's box and its carrier.
//! Bridges and function-value `invoke` methods receive boxes in erased slots and hand carriers on.

use super::*;

/// Unbox the `value_class` on the stack to its `target` carrier with `unbox-impl`; a `nullable`
/// carrier takes a null past the instance call.
pub(super) fn emit_value_class_unbox_adapter(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    value_class: TypeName,
    target: Ty,
    nullable: bool,
) {
    let value_class = value_class_spelling(value_class);
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
    let value_class = value_class_spelling(value_class);
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

pub(super) fn value_class_spelling(value_class: TypeName) -> String {
    value_class.render()
}

#[cfg(test)]
mod tests {
    use crate::types::type_name;

    #[test]
    fn a_value_class_owner_reuses_its_rendered_spelling() {
        let owner = type_name("sample/box6044/Outer$X");
        assert_eq!(super::value_class_spelling(owner), owner.render());
        assert_eq!(super::value_class_spelling(owner), "sample/box6044/Outer$X");
    }
}
