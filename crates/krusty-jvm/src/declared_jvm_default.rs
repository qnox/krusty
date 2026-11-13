//! The `-jvm-default` mode one declaration is compiled under: its own
//! `@JvmDefaultWithCompatibility` / `@JvmDefaultWithoutCompatibility` application, else the
//! module's mode.

use krusty::ir::IrClass;
use krusty::jvm::emit_modes::JvmDefaultMode;
use krusty::types::type_name;

/// `module` unless `class` carries one of the two mode annotations.
pub(crate) fn of_class(class: &IrClass, module: JvmDefaultMode) -> JvmDefaultMode {
    module.declared(
        class
            .applied_annotations
            .declares(type_name("kotlin/jvm/JvmDefaultWithCompatibility")),
        class
            .applied_annotations
            .declares(type_name("kotlin/jvm/JvmDefaultWithoutCompatibility")),
    )
}
