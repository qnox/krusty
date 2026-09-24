//! JVM realization of backend-neutral local-class naming provenance.

use crate::ir::{IrFile, IrModuleSource};
use crate::types::{type_name_nested_child, TypeName};

/// Rename every local classifier to its JVM name. `facade` names the file facade of a source,
/// which roots a local classifier declared outside any classifier: its whole path nests in it.
pub(crate) fn realize(ir: &mut IrFile, facade: impl Fn(IrModuleSource) -> TypeName) {
    ir.realize_local_class_names(|source, first| type_name_nested_child(facade(source), first));
}
