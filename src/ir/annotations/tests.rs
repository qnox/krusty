use super::*;

fn checked_source(optional_expectation: bool) -> DeclarationAnnotations {
    let internal = crate::types::type_name("marks/SourceMark");
    let checked = crate::types::AppliedAnnotation {
        internal,
        values: Vec::new(),
        facts: crate::types::AnnotationSemanticFacts {
            optional_expectation,
            ..Default::default()
        },
        written: Box::new(crate::types::WrittenAnnotation {
            short_name: "SourceMark".into(),
            use_site: None,
            order: Box::new([]),
        }),
        retention: crate::types::AnnotationRetention::Source,
        targets: crate::types::AnnotationTargets::DEFAULT,
        property_sites: None,
    };
    DeclarationAnnotations::from_checked([(
        &checked,
        AppliedAnnotation {
            internal,
            values: Vec::new(),
        },
    )])
}

#[test]
fn source_presence_survives_without_payload_or_metadata_flags() {
    let annotations = checked_source(false);
    assert!(annotations.declares_annotations());
    assert!(annotations.has_source_application());
    assert!(annotations.retains_none());
    assert!(!annotations.declares_metadata_annotations());
    assert!(!annotations.has_suppressed_source_application());
}

#[test]
fn erased_optional_source_application_has_no_carrier_presence() {
    let annotations = checked_source(true);
    assert!(!annotations.declares_annotations());
    assert!(!annotations.has_source_application());
    assert!(annotations.retains_none());
    assert!(!annotations.declares_metadata_annotations());
}
