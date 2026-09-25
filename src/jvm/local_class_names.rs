//! JVM realization of backend-neutral local-class naming provenance.

use crate::ir::{IrFile, IrModuleSource};
use crate::types::{type_name_nested_child, TypeName};

/// Rename every local classifier to its JVM name and name every source callable reference's
/// class. `facade` names the file facade of a source, which roots a local classifier or reference
/// declared outside any classifier: its whole path nests in it.
pub(crate) fn realize(ir: &mut IrFile, facade: impl Fn(IrModuleSource) -> TypeName) {
    ir.realize_local_class_names(|source, first| type_name_nested_child(facade(source), first));
}

/// The class a source callable reference at `expression` compiles to, as [`realize`] named it.
/// `None` for a reference the naming walk never saw, one lowering synthesized.
pub(crate) fn callable_reference_name(ir: &IrFile, expression: u32) -> Option<TypeName> {
    ir.callable_reference_names.get(&expression).copied()
}
