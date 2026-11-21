//! Where a library object keeps its instance: one for the whole program, or one on every thread
//! for an object annotated `kotlin.native.concurrent.ThreadLocal`. The installed class records the
//! annotation as a source object's applications do, and the target realizes the storage.

use crate::ir::{AppliedAnnotation, DeclarationAnnotations, RetainedAnnotation};
use crate::metadata::klib_ir::tree::KlibIrClass;

/// The applications of `object` that decide where its instance is kept, joined to their declared
/// classifiers: its `ThreadLocal` annotation, when it has one.
pub(super) fn instance_annotations(object: &KlibIrClass) -> DeclarationAnnotations {
    let thread_local = crate::types::wk::thread_local();
    let per_thread = object.base.annotations.iter().any(|annotation| {
        super::klib_types::annotation_classifier(annotation) == Some(thread_local)
    });
    if !per_thread {
        return DeclarationAnnotations::default();
    }
    DeclarationAnnotations::new(vec![RetainedAnnotation {
        retention: crate::types::AnnotationRetention::Binary,
        annotation: AppliedAnnotation {
            internal: thread_local,
            values: Vec::new(),
            written_order: None,
        },
        facts: crate::types::AnnotationSemanticFacts::default(),
    }])
}
