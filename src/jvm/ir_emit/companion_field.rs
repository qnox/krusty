//! A class's `Companion` field: its declaration and its initialization in `<clinit>`.

use super::*;

/// `static final` plus the companion object's own visibility, as kotlinc writes the outer class's
/// `Companion` field: a `private companion object` keeps its field private, a protected one
/// protected. An interface field is always public.
pub(super) fn companion_field_access(ir: &IrFile, class: &IrClass, companion: TypeName) -> u16 {
    const STATIC_FINAL: u16 = 0x0018;
    if class.is_interface {
        return STATIC_FINAL | 0x0001;
    }
    STATIC_FINAL
        | match ir.class_visibilities.get(&companion) {
            Some(crate::types::Visibility::Private) => 0x0002,
            Some(crate::types::Visibility::Protected) => 0x0004,
            _ => 0x0001,
        }
}

pub(super) fn add_companion_field(cw: &mut ClassWriter, class: &IrClass) {
    let Some(companion) = class.companion_class else {
        return;
    };
    cw.add_field(
        0x0019,
        companion.nested_segment_ref(),
        &format!("L{};", companion.render()),
    );
}

pub(super) fn emit_companion_init(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    owner: &str,
    class: &IrClass,
) {
    let Some(companion) = class.companion_class else {
        return;
    };
    let companion_name = companion.render();
    let descriptor = format!("L{companion_name};");
    // An INTERFACE's companion self-hosts its singleton (`static final $$INSTANCE`, built in the
    // companion's own `<clinit>`); the interface's `Companion` field merely aliases it.
    if is_jvm_interface(class) {
        let instance = cw.fieldref(&companion_name, "$$INSTANCE", &descriptor);
        code.getstatic(instance, 1);
        let field = cw.fieldref(owner, companion.nested_segment_ref(), &descriptor);
        code.putstatic(field, 1);
        return;
    }
    let classifier = cw.class_ref(&companion_name);
    code.new_obj(classifier);
    code.dup();
    code.aconst_null();
    let constructor = cw.methodref(
        &companion_name,
        "<init>",
        "(Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
    );
    code.invokespecial(constructor, 1, 0);
    let field = cw.fieldref(owner, companion.nested_segment_ref(), &descriptor);
    code.putstatic(field, 1);
}
