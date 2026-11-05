//! Physical companion storage named within its already-selected holder.

use crate::ir::IrFile;
use crate::types::TypeName;

pub(super) fn field_name(holder: TypeName, companion: TypeName) -> &'static str {
    companion
        .nested_segment_within(holder)
        .expect("a recorded companion belongs to its exact holder")
}

/// The current file's explicit holder edge, independent of the classifier's JVM spelling.
pub(super) fn holder(ir: &IrFile, companion: TypeName) -> Option<TypeName> {
    ir.classes
        .iter()
        .find(|holder| holder.companion_class == Some(companion))
        .map(|holder| holder.fq_name)
}
