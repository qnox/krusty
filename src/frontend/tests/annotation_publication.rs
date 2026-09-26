use super::*;

#[test]
fn pass_one_retains_checked_classifier_annotation_values_by_stable_identity() {
    let source = "annotation class Mark(val text: String, val count: Int, val enabled: Boolean)\n\
                  @Mark(count = 3, enabled = true, text = \"kept\")\n\
                  class Subject { fun discardedBody(): String = \"not retained\" }\n";
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Annotations")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let subject = index
        .classifier_declaration(crate::types::type_name("Subject"))
        .expect("stable subject declaration");

    assert_eq!(
        index.declaration_applied_annotations(subject),
        [crate::types::ResolvedAnnotation {
            annotation: crate::types::type_name("Mark"),
            arguments: vec![
                (
                    "text".to_owned(),
                    crate::types::AnnotationValue::string("kept"),
                ),
                ("count".to_owned(), crate::types::AnnotationValue::Int(3)),
                (
                    "enabled".to_owned(),
                    crate::types::AnnotationValue::Boolean(true),
                ),
            ],
        }]
    );
    assert_eq!(
        index.declaration_annotation_string_arguments(subject, 0),
        [Box::<str>::from("kept")]
    );
}

#[test]
fn classifier_annotation_publication_keeps_source_ordinals_when_an_earlier_binding_is_absent() {
    let source = "annotation class Mark(val text: String)\n\
                  @Missing @Mark(\"kept\") class Subject\n";
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Annotations")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        ["unresolved reference 'Missing'."]
    );
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let subject = index
        .classifier_declaration(crate::types::type_name("Subject"))
        .expect("stable subject declaration");

    assert_eq!(
        index.declaration_applied_annotations(subject),
        [crate::types::ResolvedAnnotation {
            annotation: crate::types::type_name("Mark"),
            arguments: vec![(
                "text".to_owned(),
                crate::types::AnnotationValue::string("kept"),
            )],
        }]
    );
    assert_eq!(
        index.declaration_annotation_string_arguments(subject, 0),
        [Box::<str>::from("kept")]
    );
}

#[test]
fn classifier_annotation_publication_reads_forward_source_constants_from_the_stable_index() {
    let source = "annotation class Mark(val text: String)\n\
                  @Mark(TEXT) class Subject\n\
                  const val TEXT = \"kept\"\n";
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Annotations")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let subject = index
        .classifier_declaration(crate::types::type_name("Subject"))
        .expect("stable subject declaration");

    assert_eq!(
        index.declaration_applied_annotations(subject),
        [crate::types::ResolvedAnnotation {
            annotation: crate::types::type_name("Mark"),
            arguments: vec![(
                "text".to_owned(),
                crate::types::AnnotationValue::string("kept"),
            )],
        }]
    );
}

#[test]
fn classifier_annotation_publication_covers_nested_stable_declarations() {
    let source = "annotation class Mark(val text: String)\n\
                  class Outer { @Mark(\"nested\") class Nested }\n";
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Annotations")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let nested = index
        .classifier_declaration(crate::types::type_name("Outer$Nested"))
        .expect("stable nested declaration");

    assert_eq!(
        index.declaration_applied_annotations(nested),
        [crate::types::ResolvedAnnotation {
            annotation: crate::types::type_name("Mark"),
            arguments: vec![(
                "text".to_owned(),
                crate::types::AnnotationValue::string("nested"),
            )],
        }]
    );
}
