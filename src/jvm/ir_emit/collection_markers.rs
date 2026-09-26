//! The Kotlin collection marker interfaces a classifier implements.
//!
//! kotlinc adds `KMappedMarker`, or a mutable collection's `KMutableX`, for each direct supertype
//! that is a Kotlin collection classifier, after the declared interfaces and in both the interface
//! table and the class `Signature`. `TypeIntrinsics.isMutableList` and its siblings read them at
//! run time, so a class without them is taken for a Java collection, which is always mutable.

use crate::ir::{IrClass, IrFile};
use crate::jvm::classfile::ClassWriter;
use crate::jvm::type_intrinsics::collection_marker;

/// The markers in kotlinc's order: first appearance among the direct supertypes, each once.
pub(super) fn marker_interfaces(ir: &IrFile, class: &IrClass) -> Vec<&'static str> {
    let mut markers = Vec::new();
    for supertype in &class.supertypes {
        let Some(marker) = supertype
            .non_null()
            .obj_internal()
            .and_then(|classifier| ir.mapped_collection(classifier))
            .map(collection_marker)
        else {
            continue;
        };
        if !markers.contains(&marker) {
            markers.push(marker);
        }
    }
    markers
}

/// Add the classifier's declared interfaces and then its collection markers.
pub(super) fn add_interfaces(cw: &mut ClassWriter, ir: &IrFile, class: &IrClass) {
    for interface in class.interfaces.iter_rendered() {
        cw.add_interface(&interface);
    }
    for marker in marker_interfaces(ir, class) {
        cw.add_interface(marker);
    }
}

/// A class `Signature` with the collection markers appended to its interfaces.
pub(super) fn with_markers(
    signature: Option<String>,
    ir: &IrFile,
    class: &IrClass,
) -> Option<String> {
    signature.map(|mut signature| {
        for marker in marker_interfaces(ir, class) {
            signature.push('L');
            signature.push_str(marker);
            signature.push(';');
        }
        signature
    })
}
