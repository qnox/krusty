//! The marker interfaces kotlinc adds for a classifier's direct supertypes.
//!
//! After the declared interfaces, in both the interface table and the class `Signature`, kotlinc
//! adds, each once and in order of first appearance among the direct supertypes:
//! - `KMappedMarker`, or a mutable collection's `KMutableX`, for a Kotlin collection classifier.
//!   `TypeIntrinsics.isMutableList` and its siblings read them at run time, so a class without them
//!   is taken for a Java collection, which is always mutable.
//! - `SuspendFunction` for a suspend function type. The coroutine runtime reads it to tell a
//!   suspend function value from a plain `Function{N+1}`.

use crate::ir::{IrClass, IrFile};
use crate::jvm::classfile::ClassWriter;
use crate::jvm::type_intrinsics::collection_marker;
use crate::types::Ty;

const SUSPEND_FUNCTION_MARKER: &str = "kotlin/coroutines/jvm/internal/SuspendFunction";

/// The markers in kotlinc's order: first appearance among the direct supertypes, each once.
pub(super) fn marker_interfaces(ir: &IrFile, class: &IrClass) -> Vec<&'static str> {
    let mut markers = Vec::new();
    for supertype in &class.supertypes {
        let Some(classifier) =
            crate::libraries::function_classifiers::supertype_classifier(supertype.non_null())
                .obj_internal()
        else {
            continue;
        };
        let marker = if let Some(collection) = ir.mapped_collection(classifier) {
            collection_marker(collection)
        } else if crate::libraries::function_classifiers::classifier(classifier)
            .is_some_and(|function| function.is_suspend() && !function.is_reflective())
        {
            SUSPEND_FUNCTION_MARKER
        } else {
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
    for interface in declared_interfaces(class) {
        cw.add_interface(&interface);
    }
    for marker in marker_interfaces(ir, class) {
        cw.add_interface(marker);
    }
}

/// The declared interfaces in source order. A function-type supertype is not a nominal interface of
/// the class; it stands at its written position as the function interface that realizes it
/// (`Function{N+1}` for a suspend one, `FunctionN` past 22 parameters).
fn declared_interfaces(class: &IrClass) -> Vec<String> {
    if !class
        .supertypes
        .iter()
        .any(|supertype| matches!(supertype.non_null(), Ty::Fun(_)))
    {
        return class.interfaces.iter_rendered().collect();
    }
    class
        .supertypes
        .iter()
        .filter_map(|supertype| match supertype.non_null() {
            Ty::Fun(function) => Some(
                crate::jvm::names::function_interface_internal_name(
                    function.params.len() + usize::from(function.suspend),
                )
                .to_owned(),
            ),
            nominal => nominal
                .obj_internal()
                .filter(|classifier| class.interfaces.contains_name(*classifier))
                .map(|classifier| classifier.render()),
        })
        .collect()
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
