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
    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    let missing = u32::try_from(source.find("Missing").expect("missing annotation spelling"))
        .expect("source coordinate fits u32");
    assert_eq!(diagnostics.diags[0].file, 0);
    assert_eq!(
        diagnostics.diags[0].span,
        crate::diag::Span::new(missing, missing + "Missing".len() as u32)
    );
    assert_eq!(diagnostics.diags[0].msg, "unresolved reference 'Missing'.");
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let subject = index
        .classifier_declaration(crate::types::type_name("Subject"))
        .expect("stable subject declaration");
    assert_eq!(
        index
            .checked_declaration_annotation_occurrences(subject)
            .expect("checked occurrences")
            .iter()
            .map(|checked| (checked.identity, checked.application.is_some()))
            .collect::<Vec<_>>(),
        [(None, false), (Some(crate::types::type_name("Mark")), true),]
    );

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

#[test]
fn classifier_annotation_publication_does_not_bind_unrelated_local_declarations() {
    let source = "fun box(): String { class Local; return \"OK\" }\n";
    let mut diagnostics = DiagSink::new();
    let _ = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("UnannotatedLocal")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn classifier_annotation_publication_ignores_local_inventory_beside_an_annotated_class() {
    let source = "annotation class Mark(val text: String)\n\
                  @Mark(\"kept\") class Subject\n\
                  fun box(): String { class Local; return \"OK\" }\n";
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("AnnotatedAndLocal")],
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
fn nested_classifier_annotation_uses_its_enclosing_classifier_scope() {
    let source = "annotation class Mark(val text: String)\n\
                  class Outer {\n\
                      companion object { private const val TEXT = \"kept\" }\n\
                      @Mark(TEXT) class Nested\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("NestedAnnotation")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let annotation = analysis.files[0]
        .decls
        .iter()
        .find_map(|declaration| match analysis.files[0].decl(*declaration) {
            crate::ast::Decl::Class(class) if class.name == "Outer.Nested" => {
                class.annotations.first()
            }
            _ => None,
        })
        .expect("nested class annotation");
    let applied = analysis.types[0]
        .as_ref()
        .expect("inspection analysis")
        .applied_annotation(annotation)
        .expect("Pass 1 application projected onto its source occurrence");
    assert_eq!(applied.internal, crate::types::type_name("Mark"));
    assert_eq!(
        applied.values,
        [(
            "text".to_owned(),
            crate::types::AnnotationValue::string("kept"),
        )]
    );
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
                crate::types::AnnotationValue::string("kept"),
            )],
        }]
    );
}

#[test]
fn target_excluded_classifier_annotation_keeps_file_suppression_and_no_semantic_payload() {
    let common = "// WITH_STDLIB\n\
                  // LANGUAGE: +MultiPlatformProjects\n\
                  import kotlin.OptionalExpectation as MayDisappear\n\
                  @MayDisappear\n\
                  expect annotation class Optional()\n";
    let platform = "// WITH_STDLIB\n\
                    // LANGUAGE: +MultiPlatformProjects\n\
                    @file:Suppress(\"OPTIONAL_DECLARATION_USAGE_IN_NON_COMMON_SOURCE\")\n\
                    @kotlin.js.JsNoRuntime class Subject\n\
                    @Optional fun answer(): String = \"OK\"\n";
    let inputs = [
        SourceInput::kotlin(common)
            .common()
            .with_file_stem("CommonOptional"),
        SourceInput::kotlin(platform).with_file_stem("PlatformUse"),
    ];
    let mut classpath = crate::toolchain::classpath_jars_for(common);
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let platform = Box::new(
        crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(classpath),
        ))
        .expect("JVM provider initialization"),
    );
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        platform,
        &LangFeatures::from_source(common),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let classifier = index
        .classifier_declaration(crate::types::type_name("Optional"))
        .expect("target-less optional expectation must remain a common semantic declaration");
    let constructor = index
        .owned_declaration(classifier, crate::fir::DeclarationKind::Constructor, 0)
        .expect("optional annotation constructor declaration");
    assert!(
        index.signature(constructor).is_some(),
        "Pass 2 must consume the finalized annotation constructor signature"
    );
    let subject = index
        .classifier_declaration(crate::types::type_name("Subject"))
        .expect("stable annotated subject");
    assert_eq!(
        index
            .checked_declaration_annotation_occurrences(subject)
            .expect("checked target-excluded annotation")
            .iter()
            .map(|checked| {
                (
                    checked.identity,
                    checked.application.is_some(),
                    checked.target_excluded,
                )
            })
            .collect::<Vec<_>>(),
        [(None, false, true)]
    );
    assert!(index.declaration_annotations(subject).is_empty());
    assert!(index.declaration_applied_annotations(subject).is_empty());

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}
