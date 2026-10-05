//! A class's `Companion` field: its declaration and its initialization in `<clinit>`.

use super::*;

/// `static final` plus the companion object's visibility, as kotlinc writes the outer class's
/// `Companion` field: a `private companion object` keeps its field private. kotlinc also keeps a
/// protected one's field protected, but reads it from a class outside the hierarchy through a
/// `$s<hash>` accessor on the subclass that grants access; until that accessor is ported, the
/// field stays public so those reads link. An interface field is always public.
pub(super) fn companion_field_access(ir: &IrFile, class: &IrClass, companion: TypeName) -> u16 {
    const STATIC_FINAL: u16 = 0x0018;
    if class.is_interface {
        return STATIC_FINAL | 0x0001;
    }
    STATIC_FINAL
        | match ir.class_visibilities.get(&companion) {
            Some(crate::types::Visibility::Private) => 0x0002,
            _ => 0x0001,
        }
}

/// The class whose PRIVATE `Companion` field holds singleton `companion`. Another JVM class (a
/// nested or inner class, an object expression, the companion itself) may not read that field, so
/// kotlinc's `SyntheticAccessorLowering` reads it through the owner's `access$get<Companion>$p`.
fn private_companion_field_owner(ir: &IrFile, companion: TypeName) -> Option<TypeName> {
    let owner = companion.nested_owner()?;
    let class = ir.classes.get(ir.class_id_by_name(owner)? as usize)?;
    (class.companion_class == Some(companion)
        && companion_field_access(ir, class, companion) & 0x0002 != 0)
        .then_some(owner)
}

/// The private `Companion` field read by `expression`, as (owner, companion): a checked singleton
/// value, or a lowered read of the outer class's `Companion` field.
pub(super) fn private_companion_read(
    ir: &IrFile,
    expression: &IrExpr,
) -> Option<(TypeName, TypeName)> {
    let companion = match expression {
        IrExpr::SingletonValue { classifier } => *classifier,
        IrExpr::StaticInstance { owner, ty, .. } if owner != ty => {
            ir.classes.get(*ty as usize)?.fq_name
        }
        IrExpr::ExternalStaticInstance { owner, ty, .. } if owner != ty => *ty,
        _ => return None,
    };
    private_companion_field_owner(ir, companion).map(|owner| (owner, companion))
}

/// The name and descriptor of the accessor that reads `companion`'s private `Companion` field.
pub(super) fn companion_instance_accessor(companion: TypeName) -> (String, String) {
    (
        format!(
            "access${}$p",
            property_getter_name(companion.nested_segment_ref())
        ),
        format!("()L{};", companion.render()),
    )
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
