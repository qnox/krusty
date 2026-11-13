//! The `-jvm-default` mode one declaration is compiled under: its own
//! `@JvmDefaultWithCompatibility` / `@JvmDefaultWithoutCompatibility` application, else the
//! module's mode.

use krusty::ir::IrClass;
use krusty::jvm::emit_modes::JvmDefaultMode;
use krusty::types::type_name;

/// `module` unless `class` carries one of the two mode annotations.
pub(crate) fn of_class(class: &IrClass, module: JvmDefaultMode) -> JvmDefaultMode {
    of_declarations(
        module,
        class
            .applied_annotations
            .declares(type_name("kotlin/jvm/JvmDefaultWithCompatibility")),
        class
            .applied_annotations
            .declares(type_name("kotlin/jvm/JvmDefaultWithoutCompatibility")),
    )
}

/// `module` unless these resolved annotations carry a mode override.
pub(crate) fn of_annotations(
    annotations: impl IntoIterator<Item = krusty::types::TypeName>,
    module: JvmDefaultMode,
) -> JvmDefaultMode {
    let mut with_compatibility = false;
    let mut without_compatibility = false;
    for annotation in annotations {
        with_compatibility |= annotation == type_name("kotlin/jvm/JvmDefaultWithCompatibility");
        without_compatibility |=
            annotation == type_name("kotlin/jvm/JvmDefaultWithoutCompatibility");
    }
    of_declarations(module, with_compatibility, without_compatibility)
}

fn of_declarations(
    module: JvmDefaultMode,
    with_compatibility: bool,
    without_compatibility: bool,
) -> JvmDefaultMode {
    module.declared(with_compatibility, without_compatibility)
}
