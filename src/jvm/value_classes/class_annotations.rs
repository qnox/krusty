//! The class-file annotations of a value class.
//!
//! On the JVM a value class is realized as `@JvmInline`. kotlinc's class codegen writes that
//! annotation for a value class that does not declare it (the deprecated `inline class W(val x: Int)`
//! spelling), after the declared annotations. It is a representation fact of the JVM class file only:
//! the class's `@Metadata` records the declared annotations, so a legacy inline class keeps
//! `hasAnnotations` clear and lists no annotation there.

use crate::ir::{AppliedAnnotation, DeclarationAnnotations, IrClass, RetainedAnnotation};
use crate::types::{type_name, AnnotationRetention};
use std::borrow::Cow;

/// The annotations `class`'s class file carries: the declared ones, followed by `@JvmInline` for a
/// value class that does not declare it.
pub(crate) fn class_file_annotations(class: &IrClass) -> Cow<'_, DeclarationAnnotations> {
    let jvm_inline = type_name("kotlin/jvm/JvmInline");
    let declared = &class.applied_annotations;
    if !class.is_value
        || declared
            .iter()
            .any(|retained| retained.annotation.internal == jvm_inline)
    {
        return Cow::Borrowed(declared);
    }
    let implied = RetainedAnnotation {
        retention: AnnotationRetention::Runtime,
        annotation: AppliedAnnotation {
            internal: jvm_inline,
            values: Vec::new(),
        },
        // A class-file representation, not a source application: it carries no semantic
        // facts such as hidden deprecation.
        facts: Default::default(),
    };
    Cow::Owned(DeclarationAnnotations::new(
        declared.iter().cloned().chain([implied]).collect(),
    ))
}
