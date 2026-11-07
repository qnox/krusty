//! A constructor Kotlin hides that Java may still call ([`crate::ir::IrSecondaryCtor::platform_visible`],
//! kotlinc's no-arg plugin constructor) carries `@java.lang.Deprecated` next to its
//! `@Deprecated(level = HIDDEN)`. kotlinc's no-arg plugin adds it, and that annotation is what
//! keeps the HIDDEN constructor out of `ACC_SYNTHETIC`, so Java and reflection-driven frameworks
//! can still call it (KT-80649). Kotlin metadata records it as well, after the checked
//! applications.

use crate::ir::{AppliedAnnotation, IrFile, RetainedAnnotation};

/// Annotate every platform-visible constructor with `@java.lang.Deprecated`.
pub(crate) fn realize(ir: &mut IrFile) {
    let records = &mut ir.class_declarations;
    for class in &mut ir.classes {
        let mut record = records
            .get_mut(&class.fq_name_id())
            .and_then(Option::as_mut);
        for (ordinal, constructor) in class
            .secondary_ctors
            .iter_mut()
            .enumerate()
            .filter(|(_, constructor)| constructor.platform_visible)
        {
            constructor
                .annotations
                .push_metadata_visible(RetainedAnnotation {
                    retention: crate::types::AnnotationRetention::Runtime,
                    annotation: AppliedAnnotation {
                        written_order: None,
                        internal: crate::types::type_name("java/lang/Deprecated"),
                        values: Vec::new(),
                    },
                    facts: crate::types::AnnotationSemanticFacts::default(),
                });
            if let Some(record) = record.as_deref_mut() {
                record.refresh_secondary_constructor_annotations(ordinal, &constructor.annotations);
            }
        }
    }
}
